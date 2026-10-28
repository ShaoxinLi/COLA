use super::inbound;
use crate::{
    app::{AgentService, ModelClient, SessionStore},
    domain::{AssistantMessage, Decision, NonEmptyText, SessionId, UserMessage},
};
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    header,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, io, net::TcpListener, thread};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
    task::JoinSet,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostMessage {
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {}
#[derive(Serialize)]
struct Created {
    session: inbound::SessionDto,
}
#[derive(Serialize)]
struct Listed {
    sessions: Vec<inbound::SessionDto>,
}
#[derive(Serialize)]
struct Chatted {
    session_id: String,
    messages: Vec<inbound::MessageDto>,
}
#[derive(Serialize)]
struct Failure {
    error: inbound::FailureDto,
}

enum Command {
    New(oneshot::Sender<Reply>),
    List(oneshot::Sender<Reply>),
    Chat(SessionId, UserMessage, oneshot::Sender<Reply>),
    Recover(SessionId, oneshot::Sender<Reply>),
}

/// An HTTP adapter that keeps one service on its caller's thread.
/// A private networking thread reads requests while service operations execute serially.
pub struct WebApi<M, S> {
    service: AgentService<M, S>,
}
impl<M, S> WebApi<M, S>
where
    M: ModelClient<AssistantMessage> + ModelClient<Decision>,
    S: SessionStore,
{
    /// Takes ownership without requiring `Send` or rebuilding collaborators.
    pub fn new(service: AgentService<M, S>) -> Self {
        Self { service }
    }
    /// Serves indefinitely on an already-bound listener. Fatal networking errors propagate.
    /// Call outside a Tokio runtime because command reception blocks the calling thread.
    /// A disconnected client does not cancel enqueued work. There is no shutdown deadline.
    pub fn run(self, listener: TcpListener) -> io::Result<()> {
        let (sender, mut commands) = mpsc::channel(1);
        let network = thread::Builder::new()
            .name("cola-http".into())
            .spawn(move || {
                listener.set_nonblocking(true)?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    let mut connections = JoinSet::new();
                    loop {
                        tokio::select! {
                            _ = sender.closed() => return Ok(()),
                            accepted = listener.accept() => {
                                let (stream, _) = match accepted {
                                    Ok(connection) => connection,
                                    Err(error) if matches!(error.kind(),
                                        io::ErrorKind::ConnectionAborted
                                            | io::ErrorKind::ConnectionReset
                                            | io::ErrorKind::Interrupted
                                    ) => continue,
                                    Err(error) => return Err(error),
                                };
                                let sender = sender.clone();
                                connections.spawn(async move {
                                    let service = service_fn(move |request| {
                                        handle_request(sender.clone(), request)
                                    });
                                    let Ok(parts) = http1::Builder::new()
                                        .half_close(true)
                                        .serve_connection(TokioIo::new(stream), service)
                                        .without_shutdown()
                                        .await
                                    else {
                                        return;
                                    };
                                    let mut stream = parts.io.into_inner();
                                    // Send FIN before draining unread input so an early rejection
                                    // reaches the client without a reset or a wait for its body.
                                    if stream.shutdown().await.is_ok() {
                                        let mut buffer = [0; 8192];
                                        while let Ok(read) = stream.read(&mut buffer).await {
                                            if read == 0 {
                                                break;
                                            }
                                        }
                                    }
                                });
                            }
                            Some(finished) = connections.join_next() => {
                                finished.map_err(io::Error::other)?;
                            }
                        }
                    }
                })
            })?;
        while let Some(command) = commands.blocking_recv() {
            let (sender, reply) = match command {
                Command::New(sender) => {
                    let reply = match self.service.new_session() {
                        Ok(session) => (
                            201,
                            None,
                            encode(&Created {
                                session: inbound::session_dto(&session),
                            }),
                        ),
                        Err(e) => service_error(&e),
                    };
                    (sender, reply)
                }
                Command::List(sender) => {
                    let reply = match self.service.list_sessions() {
                        Ok(sessions) => (
                            200,
                            None,
                            encode(&Listed {
                                sessions: sessions.iter().map(inbound::session_dto).collect(),
                            }),
                        ),
                        Err(e) => service_error(&e),
                    };
                    (sender, reply)
                }
                Command::Recover(id, sender) => {
                    let reply = match self.service.recover(&id) {
                        Ok(report) => (
                            200,
                            None,
                            encode(&Created {
                                session: inbound::session_dto(report.archive()),
                            }),
                        ),
                        Err(e) => service_error(&e),
                    };
                    (sender, reply)
                }
                Command::Chat(id, user, sender) => {
                    let reply = match self.service.chat(&id, user) {
                        Ok(turn) => (
                            200,
                            None,
                            encode(&Chatted {
                                session_id: id.as_str().into(),
                                messages: inbound::message_dtos(turn.messages()),
                            }),
                        ),
                        Err(e) => service_error(&e),
                    };
                    (sender, reply)
                }
            };
            let _ = sender.send(reply);
        }
        network
            .join()
            .map_err(|_| io::Error::other("HTTP networking thread panicked"))?
    }
}

async fn handle_request(
    sender: mpsc::Sender<Command>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let (reply_sender, reply_receiver) = oneshot::channel();
    let reply = match command(request, reply_sender).await {
        Err(reply) => reply,
        Ok(command) => {
            if sender.send(command).await.is_err() {
                input_error(500, "internal_failure", "Service unavailable")
            } else {
                reply_receiver
                    .await
                    .unwrap_or_else(|_| input_error(500, "internal_failure", "Service unavailable"))
            }
        }
    };
    let (status, allow, body) = reply;
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(allow) = allow {
        response = response.header(header::ALLOW, allow);
    }
    Ok(response
        .body(Full::new(Bytes::from(body)))
        .expect("constant HTTP response metadata"))
}

async fn command(
    request: Request<Incoming>,
    sender: oneshot::Sender<Reply>,
) -> Result<Command, Reply> {
    let route = route(request.uri().path())
        .map_err(|_| input_error(400, "invalid_input", "Invalid session path"))?;
    let method = request.method().clone();
    match (route, method.as_str()) {
        (Route::Unknown, _) => Err(input_error(404, "route_not_found", "Route not found")),
        (Route::Sessions, "GET") => Ok(Command::List(sender)),
        (Route::Sessions, "POST") => {
            let bytes = body(request, true).await?;
            if !bytes.is_empty() {
                object::<Create>(&bytes)?;
            }
            Ok(Command::New(sender))
        }
        (Route::Recover(id), "POST") => {
            let bytes = body(request, true).await?;
            if !bytes.is_empty() {
                object::<Create>(&bytes)?;
            }
            Ok(Command::Recover(id, sender))
        }
        (Route::Messages(id), "POST") => {
            let bytes = body(request, false).await?;
            let post = object::<PostMessage>(&bytes)?;
            let user = NonEmptyText::new(post.text)
                .map(UserMessage::new)
                .map_err(|_| input_error(400, "invalid_input", "Invalid message input"))?;
            Ok(Command::Chat(id, user, sender))
        }
        (route, _) => {
            let mut reply = input_error(405, "method_not_allowed", "Method not allowed");
            reply.1 = Some(match route {
                Route::Sessions => "GET, POST",
                Route::Messages(_) | Route::Recover(_) => "POST",
                Route::Unknown => unreachable!(),
            });
            Err(reply)
        }
    }
}
type Reply = (u16, Option<&'static str>, Vec<u8>);
fn encode(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON DTO serialization")
}
fn input_error(status: u16, code: &'static str, message: &'static str) -> Reply {
    (
        status,
        None,
        encode(&Failure {
            error: inbound::input_failure(code, message),
        }),
    )
}
fn service_error(error: &crate::app::AgentError) -> Reply {
    let failure = inbound::classify(error);
    let status = match failure.class {
        inbound::FailureClass::NotFound => 404,
        inbound::FailureClass::Conflict => 409,
        inbound::FailureClass::Capacity => 422,
        inbound::FailureClass::Refused | inbound::FailureClass::IterationLimit => 422,
        inbound::FailureClass::Incomplete | inbound::FailureClass::Model => 502,
        inbound::FailureClass::Timeout => 504,
        inbound::FailureClass::Store | inbound::FailureClass::Internal => 500,
    };
    (status, None, encode(&Failure { error: failure.dto }))
}
async fn body(request: Request<Incoming>, allow_empty: bool) -> Result<Bytes, Reply> {
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > inbound::INPUT_LIMIT as u64)
    {
        return Err(input_error(413, "body_too_large", "Request body too large"));
    }
    let mut headers = request.headers().get_all(header::CONTENT_TYPE).iter();
    let json = headers.next().is_some_and(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    }) && headers.next().is_none();
    let bytes = Limited::new(request.into_body(), inbound::INPUT_LIMIT)
        .collect()
        .await
        .map_err(|error| {
            if error.is::<LengthLimitError>() {
                input_error(413, "body_too_large", "Request body too large")
            } else {
                input_error(400, "invalid_input", "Could not read request body")
            }
        })?
        .to_bytes();
    if allow_empty && bytes.is_empty() {
        return Ok(bytes);
    }
    if !json {
        return Err(input_error(
            415,
            "unsupported_media_type",
            "JSON media type required",
        ));
    }
    Ok(bytes)
}
fn object<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Reply> {
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(input_error(400, "invalid_input", "JSON object required"));
    }
    serde_json::from_slice(bytes)
        .map_err(|_| input_error(400, "invalid_input", "Invalid JSON object"))
}
enum Route {
    Sessions,
    Messages(SessionId),
    Recover(SessionId),
    Unknown,
}
fn route(url: &str) -> Result<Route, ()> {
    let path = url.split('?').next().unwrap_or(url);
    if path == "/sessions" {
        return Ok(Route::Sessions);
    }
    let Some(tail) = path.strip_prefix("/sessions/") else {
        return Ok(Route::Unknown);
    };
    let (id, recovery) = if let Some(id) = tail.strip_suffix("/messages") {
        (id, false)
    } else if let Some(id) = tail.strip_suffix("/recover") {
        (id, true)
    } else {
        return Ok(Route::Unknown);
    };
    if id.contains('/') || id.is_empty() {
        return Ok(Route::Unknown);
    }
    let mut decoded = Vec::new();
    let mut bytes = id.bytes();
    while let Some(b) = bytes.next() {
        decoded.push(if b == b'%' {
            let high = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
            let low = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
            (high * 16 + low) as u8
        } else {
            b
        });
    }
    if decoded.contains(&b'/') || decoded.contains(&b'\\') {
        return Err(());
    }
    let id = String::from_utf8(decoded).map_err(|_| ())?;
    let id = SessionId::new(id).map_err(|_| ())?;
    Ok(if recovery {
        Route::Recover(id)
    } else {
        Route::Messages(id)
    })
}
