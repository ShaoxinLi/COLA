use super::contracts::{execute, failure, success};
use cola::adapters::tools::BashTool;
use serde_json::json;
use tempfile::tempdir;

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
