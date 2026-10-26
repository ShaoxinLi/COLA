use cola::adapters::tools::{BashTool, ReadFileTool, WriteFileTool};
use cola::domain::{Tool, ToolOutcome};
use serde_json::{Value, json};

fn run(tool: &dyn Tool, arguments: Value) -> Result<String, Box<dyn std::error::Error>> {
    match tool.execute(
        arguments
            .as_object()
            .expect("example arguments are objects"),
    ) {
        ToolOutcome::Success(output) => Ok(output),
        ToolOutcome::Failure(error) => Err(error.description().as_str().into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let writer = WriteFileTool::new(directory.path())?;
    let reader = ReadFileTool::new(directory.path())?;
    let bash = BashTool::new(directory.path())?;
    let content = "Hello from COLA.\n";
    let written = run(&writer, json!({"path": "notes.txt", "content": content}))?;
    if !written.is_empty() {
        return Err("write_file returned unexpected output".into());
    }
    let read = run(&reader, json!({"path": "notes.txt"}))?;
    if read != content {
        return Err("read_file returned unexpected content".into());
    }
    print!("File content: {read}");
    let output = run(
        &bash,
        json!({"command": "items=(real tools); printf '%s %s: ' \"${items[0]}\" \"${items[1]}\"; cat notes.txt"}),
    )?;
    if output != format!("real tools: {content}") {
        return Err("bash returned unexpected output".into());
    }
    print!("Bash output: {output}");
    Ok(())
}
