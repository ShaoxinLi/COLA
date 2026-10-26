//! Synchronous host tools with an instance-specific working directory.

mod bash;
mod files;

pub use bash::BashTool;
pub use files::{ReadFileTool, WriteFileTool};

use crate::domain::{InputSchema, NonEmptyText, ToolDefinition, ToolError, ToolName, ToolOutcome};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

struct WorkingDirectory(PathBuf);

impl WorkingDirectory {
    fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().canonicalize()?;
        if !path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "tool working directory must be a directory",
            ));
        }
        Ok(Self(path))
    }

    fn resolve(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

fn definition(name: &str, description: &str, schema: Value) -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(name).expect("static tool name is valid"),
        NonEmptyText::new(description).expect("static tool description is nonblank"),
        InputSchema::new(schema).expect("static tool schema has an object root"),
    )
}

fn failure(message: impl Into<String>) -> ToolOutcome {
    ToolOutcome::Failure(ToolError::new(
        NonEmptyText::new(message.into()).expect("contextual tool error is nonblank"),
    ))
}
