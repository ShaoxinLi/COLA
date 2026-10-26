use super::{WorkingDirectory, definition, failure};
use crate::domain::{Tool, ToolDefinition, ToolOutcome};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;

const CAPTURE_LIMIT: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
}

struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Captured {
    fn read(mut reader: impl Read) -> io::Result<Self> {
        let mut captured = Self {
            bytes: Vec::new(),
            truncated: false,
        };
        let mut buffer = [0; 8192];
        loop {
            let count = match reader.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if count == 0 {
                return Ok(captured);
            }
            let retained = count.min(CAPTURE_LIMIT - captured.bytes.len());
            captured.bytes.extend_from_slice(&buffer[..retained]);
            captured.truncated |= retained < count;
        }
    }

    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            text.push_str("\n[output truncated after 65536 bytes]");
        }
        text
    }
}

/// Runs Bash synchronously with bounded output capture and no wall-clock limit.
/// Descendants holding output pipes can keep execution blocked after Bash exits.
pub struct BashTool {
    cwd: WorkingDirectory,
}

impl BashTool {
    /// Anchors execution to an existing directory. This is not confinement.
    pub fn new(cwd: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            cwd: WorkingDirectory::new(cwd)?,
        })
    }

    pub fn definition() -> ToolDefinition {
        definition(
            "bash",
            "Run Bash synchronously in the tool working directory with inherited environment and EOF on stdin. Retain the first 64 KiB of stdout and stderr, with truncation markers and lossy UTF-8 decoding. There is no wall-clock limit; commands or descendants holding output pipes can block indefinitely. The working directory is not a sandbox.",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string", "minLength": 1}},
                "required": ["command"],
                "additionalProperties": false
            }),
        )
    }
}

impl Tool for BashTool {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
        let arguments: Arguments = match serde_json::from_value(Value::Object(arguments.clone())) {
            Ok(arguments) => arguments,
            Err(error) => return failure(format!("Invalid bash arguments: {error}")),
        };
        if arguments.command.is_empty() {
            return failure("Invalid bash arguments: command must not be empty");
        }
        let mut child = match Command::new("bash")
            .args(["--noprofile", "--norc", "-c", &arguments.command])
            .current_dir(&self.cwd.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => return failure(format!("Could not start bash: {error}")),
        };
        let (status, stdout, stderr) = match capture(&mut child) {
            Ok(output) => output,
            Err(error) => return failure(error),
        };
        let stdout_text = stdout.text();
        let stderr_text = stderr.text();
        if status.success() {
            if stderr.bytes.is_empty() {
                ToolOutcome::Success(stdout_text)
            } else {
                ToolOutcome::Success(format!("[stdout]\n{stdout_text}\n[stderr]\n{stderr_text}"))
            }
        } else {
            failure(format!(
                "Bash failed with {status}\n[stdout]\n{stdout_text}\n[stderr]\n{stderr_text}"
            ))
        }
    }
}

fn terminate_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn capture(child: &mut Child) -> Result<(ExitStatus, Captured, Captured), String> {
    thread::scope(|scope| {
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let stdout_reader = match thread::Builder::new()
            .name("cola-bash-stdout".into())
            .spawn_scoped(scope, move || Captured::read(stdout))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_and_reap(child);
                return Err(format!("Could not start bash stdout reader: {error}"));
            }
        };
        let stderr_reader = match thread::Builder::new()
            .name("cola-bash-stderr".into())
            .spawn_scoped(scope, move || Captured::read(stderr))
        {
            Ok(reader) => reader,
            Err(error) => {
                terminate_and_reap(child);
                let _ = stdout_reader.join();
                return Err(format!("Could not start bash stderr reader: {error}"));
            }
        };
        let status = child.wait();
        if status.is_err() {
            terminate_and_reap(child);
        }
        let stdout = stdout_reader.join();
        let stderr = stderr_reader.join();
        let status = status.map_err(|error| format!("Could not wait for bash: {error}"))?;
        let stdout = stdout
            .map_err(|_| "Bash stdout reader panicked".to_owned())?
            .map_err(|error| format!("Could not read bash stdout: {error}"))?;
        let stderr = stderr
            .map_err(|_| "Bash stderr reader panicked".to_owned())?
            .map_err(|error| format!("Could not read bash stderr: {error}"))?;
        Ok((status, stdout, stderr))
    })
}
