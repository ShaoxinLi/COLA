use super::inbound;
use crate::{
    app::{AgentService, ModelClient, SessionStore},
    domain::{
        Assistant, AssistantMessage, Decision, NonEmptyText, SessionId, ToolCallReviewer,
        UserMessage,
    },
};
use std::io::{self, BufRead, Write};

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Reply {
    ReviewFailureAcknowledged {
        session_id: String,
        invocation_id: u64,
    },
    Created {
        session: inbound::SessionDto,
    },
    Listed {
        sessions: Vec<inbound::SessionDto>,
    },
    Selected {
        session_id: String,
    },
    Chatted {
        session_id: String,
        messages: Vec<inbound::MessageDto>,
    },
    Error {
        error: inbound::FailureDto,
    },
}
/// A persistent, synchronous JSON-line command session with one selected session.
pub struct CliApp<M, S, A, R> {
    service: AgentService<M, S, A, R>,
}
/// Accumulated command outcome. Invalid input takes precedence over service failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliExit {
    /// All commands succeeded.
    Success,
    /// At least one service command failed.
    CommandFailed,
    /// At least one line or command was invalid.
    InvalidInput,
}
impl CliExit {
    /// Conventional process exit value, without terminating the process.
    pub fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::CommandFailed => 1,
            Self::InvalidInput => 2,
        }
    }
}
impl<M, S, A, R> CliApp<M, S, A, R>
where
    M: ModelClient<AssistantMessage> + ModelClient<Decision>,
    S: SessionStore,
    A: Assistant,
    R: ToolCallReviewer,
{
    /// Takes ownership of an existing service. Collaborators need not be `Send`.
    pub fn new(service: AgentService<M, S, A, R>) -> Self {
        Self { service }
    }
    /// Runs until `/exit` or EOF, flushing each reply. Model and tool calls block input.
    /// Stream failures return immediately as I/O errors.
    pub fn run<I: BufRead, O: Write>(mut self, mut input: I, mut output: O) -> io::Result<CliExit> {
        let mut selected = None;
        let mut exit = CliExit::Success;
        while let Some(line) = read_line(&mut input)? {
            let line = match line.and_then(|b| String::from_utf8(b).map_err(|_| ())) {
                Ok(line) => line,
                Err(()) => {
                    exit = CliExit::InvalidInput;
                    write_reply(&mut output, &invalid())?;
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            if line == "/exit" {
                break;
            }
            let reply = if line == "/new" {
                self.service.new_session().map(|s| {
                    selected = Some(s.id().clone());
                    Reply::Created {
                        session: inbound::session_dto(&s),
                    }
                })
            } else if line == "/sessions" {
                self.service.list_session().map(|sessions| Reply::Listed {
                    sessions: sessions.iter().map(inbound::session_dto).collect(),
                })
            } else if let Some(id) = line.strip_prefix("/use ") {
                match SessionId::new(id) {
                    Ok(id) => {
                        let reply = Reply::Selected {
                            session_id: id.as_str().into(),
                        };
                        selected = Some(id);
                        Ok(reply)
                    }
                    Err(_) => {
                        exit = CliExit::InvalidInput;
                        Ok(invalid())
                    }
                }
            } else if let Some(rest) = line.strip_prefix("/ack-review-failure ") {
                let parsed = rest.split_once(' ').and_then(|(id, evidence)| {
                    Some((
                        id.parse::<u64>().ok().filter(|id| *id > 0)?,
                        NonEmptyText::new(evidence).ok()?,
                    ))
                });
                if let (Some(session), Some((invocation, evidence))) = (&selected, parsed) {
                    self.service
                        .acknowledge_review_failure(session, invocation, evidence, None)
                        .map(|_| Reply::ReviewFailureAcknowledged {
                            session_id: session.as_str().into(),
                            invocation_id: invocation,
                        })
                } else {
                    exit = CliExit::InvalidInput;
                    Ok(invalid())
                }
            } else if line.starts_with('/') && !line.starts_with("//") {
                exit = CliExit::InvalidInput;
                Ok(invalid())
            } else if let Some(id) = &selected {
                let text = line
                    .strip_prefix('/')
                    .filter(|_| line.starts_with("//"))
                    .unwrap_or(&line);
                let user =
                    UserMessage::new(NonEmptyText::new(text).expect("nonblank command text"));
                self.service.chat(id, user).map(|messages| Reply::Chatted {
                    session_id: id.as_str().into(),
                    messages: inbound::message_dtos(&messages),
                })
            } else {
                exit = CliExit::InvalidInput;
                Ok(invalid())
            };
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    if exit == CliExit::Success {
                        exit = CliExit::CommandFailed;
                    }
                    Reply::Error {
                        error: inbound::classify(&error).dto,
                    }
                }
            };
            write_reply(&mut output, &reply)?;
        }
        Ok(exit)
    }
}
fn invalid() -> Reply {
    Reply::Error {
        error: inbound::input_failure("invalid_input", "Invalid command input"),
    }
}
fn write_reply(output: &mut impl Write, reply: &Reply) -> io::Result<()> {
    serde_json::to_writer(&mut *output, reply).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}
fn read_line(input: &mut impl BufRead) -> io::Result<Option<Result<Vec<u8>, ()>>> {
    let mut bytes = Vec::new();
    let mut overlong = false;
    let mut seen = false;
    loop {
        let buffer = match input.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if buffer.is_empty() {
            if !seen {
                return Ok(None);
            }
            return Ok(Some(if overlong || bytes.len() > inbound::INPUT_LIMIT {
                Err(())
            } else {
                Ok(bytes)
            }));
        }
        seen = true;
        let end = buffer.iter().position(|b| *b == b'\n');
        let count = end.map_or(buffer.len(), |i| i + 1);
        if !overlong {
            let room = (inbound::INPUT_LIMIT + 2).saturating_sub(bytes.len());
            bytes.extend_from_slice(&buffer[..count.min(room)]);
            overlong = count > room;
        }
        input.consume(count);
        if end.is_some() {
            if !overlong {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            return Ok(Some(if overlong || bytes.len() > inbound::INPUT_LIMIT {
                Err(())
            } else {
                Ok(bytes)
            }));
        }
    }
}
