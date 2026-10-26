use super::{WorkingDirectory, definition, failure};
use crate::domain::{Tool, ToolDefinition, ToolOutcome};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

const READ_LIMIT: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArguments {
    path: String,
    content: String,
}

/// Reads exact UTF-8 text from regular files, up to 1 MiB.
pub struct ReadFileTool {
    cwd: WorkingDirectory,
}

impl ReadFileTool {
    /// Anchors relative paths to an existing directory. This is not confinement.
    pub fn new(cwd: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            cwd: WorkingDirectory::new(cwd)?,
        })
    }

    pub fn definition() -> ToolDefinition {
        definition(
            "read_file",
            "Read exact UTF-8 text from a regular file up to 1 MiB. Relative paths use the tool working directory; absolute paths and host symlinks are accepted. The working directory is not a sandbox.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "minLength": 1}},
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }
}

impl Tool for ReadFileTool {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
        let arguments: ReadArguments =
            match serde_json::from_value(Value::Object(arguments.clone())) {
                Ok(arguments) => arguments,
                Err(error) => return failure(format!("Invalid read_file arguments: {error}")),
            };
        if arguments.path.is_empty() {
            return failure("Invalid read_file arguments: path must not be empty");
        }
        let path = self.cwd.resolve(&arguments.path);
        let read = || -> io::Result<String> {
            if !fs::metadata(&path)?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path is not a regular file",
                ));
            }
            let mut bytes = Vec::new();
            File::open(&path)?
                .take(READ_LIMIT + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > READ_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "file exceeds the 1 MiB read limit",
                ));
            }
            String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        };
        match read() {
            Ok(content) => ToolOutcome::Success(content),
            Err(error) => failure(format!("Could not read file {}: {error}", path.display())),
        }
    }
}

/// Creates or truncates a file and writes exact UTF-8 bytes without creating parents.
/// Writes are non-atomic and a failure can leave partial content.
pub struct WriteFileTool {
    cwd: WorkingDirectory,
}

impl WriteFileTool {
    /// Anchors relative paths to an existing directory. This is not confinement.
    pub fn new(cwd: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            cwd: WorkingDirectory::new(cwd)?,
        })
    }

    pub fn definition() -> ToolDefinition {
        definition(
            "write_file",
            "Create or truncate a file and write exact UTF-8 content. Empty content is valid. Parent directories must exist. Writes are non-atomic and failures may leave partial content. Relative paths use the tool working directory; absolute paths and host symlinks are accepted. The working directory is not a sandbox.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "minLength": 1},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        )
    }
}

impl Tool for WriteFileTool {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
        let arguments: WriteArguments =
            match serde_json::from_value(Value::Object(arguments.clone())) {
                Ok(arguments) => arguments,
                Err(error) => return failure(format!("Invalid write_file arguments: {error}")),
            };
        if arguments.path.is_empty() {
            return failure("Invalid write_file arguments: path must not be empty");
        }
        let path = self.cwd.resolve(&arguments.path);
        match File::create(&path).and_then(|mut file| file.write_all(arguments.content.as_bytes()))
        {
            Ok(()) => ToolOutcome::Success(String::new()),
            Err(error) => failure(format!("Could not write file {}: {error}", path.display())),
        }
    }
}
