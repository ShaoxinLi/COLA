use cola::adapters::tools::{BashTool, ReadFileTool, WriteFileTool};
use cola::domain::{Tool, ToolDefinition, ToolOutcome};
use serde_json::{Value, json};
use std::fs;
use tempfile::tempdir;

fn execute(tool: &dyn Tool, arguments: Value) -> ToolOutcome {
    tool.execute(arguments.as_object().unwrap())
}

fn success(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Success(output) => output,
        other => panic!("expected success, got {other:?}"),
    }
}

fn failure(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Failure(error) => {
            let text = error.description().as_str();
            assert!(!text.trim().is_empty());
            text.to_owned()
        }
        other => panic!("expected failure, got {other:?}"),
    }
}

#[test]
fn definitions_describe_strict_argument_shapes() {
    let definitions: [(ToolDefinition, &str, &[&str]); 3] = [
        (BashTool::definition(), "bash", &["command"]),
        (ReadFileTool::definition(), "read_file", &["path"]),
        (
            WriteFileTool::definition(),
            "write_file",
            &["path", "content"],
        ),
    ];
    for (definition, name, fields) in definitions {
        assert_eq!(definition.name().as_str(), name);
        assert!(!definition.description().as_str().is_empty());
        let schema = definition.input_schema().document();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(fields));
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), fields.len());
        for field in fields {
            assert_eq!(properties[*field]["type"], "string");
            if *field == "content" {
                assert!(properties[*field].get("minLength").is_none());
            } else {
                assert_eq!(properties[*field]["minLength"], 1);
            }
        }
    }
    let bash_description = BashTool::definition().description().as_str().to_owned();
    assert!(bash_description.contains("no wall-clock limit"));
    assert!(bash_description.contains("descendants"));
}

#[test]
fn invalid_arguments_fail_before_effects() {
    let directory = tempdir().unwrap();
    let bash = BashTool::new(directory.path()).unwrap();
    let read = ReadFileTool::new(directory.path()).unwrap();
    let write = WriteFileTool::new(directory.path()).unwrap();
    for arguments in [
        json!({}),
        json!({"command": null}),
        json!({"command": ["touch", "effect"]}),
        json!({"command": ""}),
        json!({"command": "touch effect", "extra": true}),
    ] {
        failure(execute(&bash, arguments));
    }
    for arguments in [
        json!({}),
        json!({"path": null}),
        json!({"path": 1}),
        json!({"path": ""}),
        json!({"path": "effect", "extra": true}),
    ] {
        failure(execute(&read, arguments));
    }
    for arguments in [
        json!({}),
        json!({"path": "effect"}),
        json!({"content": "text"}),
        json!({"path": "effect", "content": null}),
        json!({"path": false, "content": "text"}),
        json!({"path": "", "content": "text"}),
        json!({"path": "effect", "content": "text", "extra": true}),
    ] {
        failure(execute(&write, arguments));
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    fs::write(directory.path().join("existing"), "keep").unwrap();
    failure(execute(
        &write,
        json!({"path": "existing", "content": "replace", "extra": true}),
    ));
    assert_eq!(
        fs::read_to_string(directory.path().join("existing")).unwrap(),
        "keep"
    );
}

#[test]
fn files_preserve_exact_text_and_literal_paths() {
    let directory = tempdir().unwrap();
    let write = WriteFileTool::new(directory.path()).unwrap();
    let read = ReadFileTool::new(directory.path()).unwrap();
    let content = "  hello\r\n世界 🦀\n\n";
    for path in ["notes.txt", " ", "$(touch injected); 'literal'.txt"] {
        assert_eq!(
            success(execute(&write, json!({"path": path, "content": content}))),
            ""
        );
        assert_eq!(
            fs::read(directory.path().join(path)).unwrap(),
            content.as_bytes()
        );
        assert_eq!(success(execute(&read, json!({"path": path}))), content);
        assert_eq!(
            success(execute(&write, json!({"path": path, "content": ""}))),
            ""
        );
        assert_eq!(success(execute(&read, json!({"path": path}))), "");
    }
    assert!(!directory.path().join("injected").exists());
    success(execute(
        &write,
        json!({"path": "notes.txt", "content": "short"}),
    ));
    assert_eq!(
        fs::read_to_string(directory.path().join("notes.txt")).unwrap(),
        "short"
    );
}

#[test]
fn file_failures_report_missing_directory_invalid_text_and_size() {
    let directory = tempdir().unwrap();
    let write = WriteFileTool::new(directory.path()).unwrap();
    let read = ReadFileTool::new(directory.path()).unwrap();
    failure(execute(
        &write,
        json!({"path": "missing/file", "content": "text"}),
    ));
    assert!(!directory.path().join("missing").exists());
    failure(execute(&read, json!({"path": "missing"})));
    failure(execute(&read, json!({"path": "."})));
    failure(execute(&write, json!({"path": ".", "content": "text"})));
    fs::write(directory.path().join("invalid"), [0xff, 0xfe]).unwrap();
    failure(execute(&read, json!({"path": "invalid"})));
    let limit = "x".repeat(1024 * 1024);
    fs::write(directory.path().join("limit"), &limit).unwrap();
    assert_eq!(success(execute(&read, json!({"path": "limit"}))), limit);
    fs::write(
        directory.path().join("oversized"),
        "x".repeat(1024 * 1024 + 1),
    )
    .unwrap();
    let error = failure(execute(&read, json!({"path": "oversized"})));
    assert!(error.len() < 1024);
}

#[test]
fn constructors_require_existing_directories() {
    let directory = tempdir().unwrap();
    let missing = directory.path().join("missing");
    let file = directory.path().join("file");
    fs::write(&file, "text").unwrap();
    for path in [&missing, &file] {
        assert!(BashTool::new(path).is_err());
        assert!(ReadFileTool::new(path).is_err());
        assert!(WriteFileTool::new(path).is_err());
    }
}

#[test]
fn instances_anchor_paths_without_changing_process_cwd() {
    let original = std::env::current_dir().unwrap();
    let parent = tempfile::tempdir_in(&original).unwrap();
    let first = parent.path().join("first");
    let second = parent.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let relative = first.strip_prefix(&original).unwrap();
    let first_write = WriteFileTool::new(relative).unwrap();
    let second_write = WriteFileTool::new(&second).unwrap();
    success(execute(
        &first_write,
        json!({"path": "same", "content": "first"}),
    ));
    success(execute(
        &second_write,
        json!({"path": "same", "content": "second"}),
    ));
    let first_read = ReadFileTool::new(&first).unwrap();
    assert_eq!(
        success(execute(&first_read, json!({"path": "same"}))),
        "first"
    );
    assert_eq!(
        success(execute(&first_read, json!({"path": second.join("same")}))),
        "second"
    );
    success(execute(
        &first_write,
        json!({"path": second.join("absolute"), "content": "outside"}),
    ));
    assert_eq!(
        fs::read_to_string(second.join("absolute")).unwrap(),
        "outside"
    );
    success(execute(
        &first_write,
        json!({"path": "../parent", "content": "parent"}),
    ));
    assert_eq!(
        fs::read_to_string(parent.path().join("parent")).unwrap(),
        "parent"
    );
    for path in [&first, &second] {
        let bash = BashTool::new(path).unwrap();
        let output = success(execute(&bash, json!({"command": "printf '%s' \"$PWD\""})));
        assert_eq!(output, path.canonicalize().unwrap().to_str().unwrap());
    }
    assert_eq!(std::env::current_dir().unwrap(), original);
}

#[cfg(unix)]
#[test]
fn files_follow_host_symlinks() {
    use std::os::unix::fs::symlink;
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("target"), "before").unwrap();
    symlink("target", directory.path().join("link")).unwrap();
    let read = ReadFileTool::new(directory.path()).unwrap();
    let write = WriteFileTool::new(directory.path()).unwrap();
    assert_eq!(success(execute(&read, json!({"path": "link"}))), "before");
    success(execute(&write, json!({"path": "link", "content": "after"})));
    assert_eq!(
        fs::read_to_string(directory.path().join("target")).unwrap(),
        "after"
    );
}

#[cfg(unix)]
#[test]
fn constructors_anchor_symlinked_directories_once() {
    use std::os::unix::fs::symlink;
    let directory = tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    let alias = directory.path().join("alias");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("source"), "first").unwrap();
    fs::write(second.join("source"), "second").unwrap();
    symlink(&first, &alias).unwrap();
    let read = ReadFileTool::new(&alias).unwrap();
    let write = WriteFileTool::new(&alias).unwrap();
    let bash = BashTool::new(&alias).unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(&second, &alias).unwrap();
    assert_eq!(success(execute(&read, json!({"path": "source"}))), "first");
    success(execute(
        &write,
        json!({"path": "created", "content": "anchored"}),
    ));
    assert_eq!(
        fs::read_to_string(first.join("created")).unwrap(),
        "anchored"
    );
    assert!(!second.join("created").exists());
    assert_eq!(
        success(execute(&bash, json!({"command": "cat source"}))),
        "first"
    );
}

#[cfg(unix)]
#[test]
fn read_rejects_fifo_before_opening() {
    let directory = tempdir().unwrap();
    let fifo = directory.path().join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let read = ReadFileTool::new(directory.path()).unwrap();
    failure(execute(&read, json!({"path": "fifo"})));
}

#[test]
fn bash_supports_arrays_eof_stdin_and_fresh_processes() {
    let directory = tempdir().unwrap();
    let bash = BashTool::new(directory.path()).unwrap();
    assert_eq!(
        success(execute(
            &bash,
            json!({"command": "items=(hello world); printf '%s:%s' \"${items[0]}\" \"${items[1]}\""})
        )),
        "hello:world"
    );
    assert_eq!(
        success(execute(
            &bash,
            json!({"command": "if read -r line; then exit 1; else printf eof; fi"})
        )),
        "eof"
    );
    assert_eq!(success(execute(&bash, json!({"command": "   \n\t"}))), "");
    assert_eq!(success(execute(&bash, json!({"command": "true"}))), "");
    success(execute(
        &bash,
        json!({"command": "export COLA_TOOL_LOCAL=fresh"}),
    ));
    assert_eq!(
        success(execute(
            &bash,
            json!({"command": "printf '%s' \"${COLA_TOOL_LOCAL-unset}\""})
        )),
        std::env::var("COLA_TOOL_LOCAL").unwrap_or_else(|_| "unset".to_owned())
    );
}

#[test]
fn bash_preserves_output_and_labels_stderr() {
    let directory = tempdir().unwrap();
    let bash = BashTool::new(directory.path()).unwrap();
    assert_eq!(
        success(execute(&bash, json!({"command": "printf '  hello\\n\\n'"}))),
        "  hello\n\n"
    );
    assert_eq!(
        success(execute(&bash, json!({"command": "printf warning >&2"}))),
        "[stdout]\n\n[stderr]\nwarning"
    );
    assert_eq!(
        success(execute(
            &bash,
            json!({"command": "printf hello; printf warning >&2"})
        )),
        "[stdout]\nhello\n[stderr]\nwarning"
    );
    assert_eq!(
        success(execute(&bash, json!({"command": "printf '\\377'"}))),
        "�"
    );
    let error = failure(execute(
        &bash,
        json!({"command": "printf partial; printf problem >&2; exit 7"}),
    ));
    for expected in ["7", "[stdout]", "partial", "[stderr]", "problem"] {
        assert!(error.contains(expected), "{error}");
    }
    failure(execute(&bash, json!({"command": "exit 2"})));
}

#[cfg(unix)]
#[test]
fn bash_reports_signal_and_partial_output() {
    let directory = tempdir().unwrap();
    let bash = BashTool::new(directory.path()).unwrap();
    let error = failure(execute(
        &bash,
        json!({"command": "printf before; kill -TERM $$"}),
    ));
    assert!(error.contains("signal"));
    assert!(error.contains("before"));
}

#[test]
fn bash_drains_both_large_streams_and_marks_truncation() {
    let directory = tempdir().unwrap();
    let bash = BashTool::new(directory.path()).unwrap();
    let overflowing_streams_then_exit_9 =
        "(printf '%200000s' '' | tr ' ' x) & (printf '%200000s' '' | tr ' ' y >&2) & wait; exit 9";
    let error = failure(execute(
        &bash,
        json!({"command": overflowing_streams_then_exit_9}),
    ));
    assert!(error.contains('9'));
    assert!(error.contains(&"x".repeat(65536)));
    assert!(error.contains(&"y".repeat(65536)));
    assert!(!error.contains(&"x".repeat(65537)));
    assert!(!error.contains(&"y".repeat(65537)));
    assert_eq!(
        error
            .matches("[output truncated after 65536 bytes]")
            .count(),
        2
    );
    assert!(error.len() < 132000);
    let exact = success(execute(&bash, json!({"command": "printf '%65536s' ''"})));
    assert_eq!(exact, " ".repeat(65536));
    let truncated = success(execute(&bash, json!({"command": "printf '%65537s' ''"})));
    assert!(truncated.starts_with(&" ".repeat(65536)));
    assert!(truncated.contains("[output truncated"));
    let split_utf8 = success(execute(
        &bash,
        json!({"command": "printf '%65535s' ''; printf '\\342\\202\\254'"}),
    ));
    assert_eq!(
        split_utf8,
        format!(
            "{}�\n[output truncated after 65536 bytes]",
            " ".repeat(65535)
        )
    );
}

#[test]
fn removed_working_directory_returns_contextual_failures() {
    let directory = tempdir().unwrap();
    let cwd = directory.path().join("removed");
    fs::create_dir(&cwd).unwrap();
    let bash = BashTool::new(&cwd).unwrap();
    let read = ReadFileTool::new(&cwd).unwrap();
    let write = WriteFileTool::new(&cwd).unwrap();
    fs::remove_dir(&cwd).unwrap();
    let error = failure(execute(&bash, json!({"command": "printf unreachable"})));
    assert!(error.to_lowercase().contains("bash"));
    for outcome in [
        execute(&read, json!({"path": "file"})),
        execute(&write, json!({"path": "file", "content": "text"})),
    ] {
        assert!(failure(outcome).contains("removed"));
    }
}
