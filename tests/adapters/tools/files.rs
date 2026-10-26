use super::{execute, failure, success};
use cola::adapters::tools::{ReadFileTool, WriteFileTool};
use serde_json::json;
use std::fs;
use tempfile::tempdir;

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
