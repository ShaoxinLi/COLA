use super::text;
use cola::domain::{NonEmptyText, SessionId, ToolCallId, ToolName, TurnId, ValueError};

#[test]
fn value_constructors_preserve_text_and_validate_identities() {
    assert_eq!(text("  hello\n").as_str(), "  hello\n");
    assert_eq!(NonEmptyText::new("\n \t"), Err(ValueError::BlankText));
    assert_eq!(SessionId::new("  "), Err(ValueError::BlankIdentity));
    assert_eq!(ToolCallId::new("\t"), Err(ValueError::BlankIdentity));
    assert_eq!(
        SessionId::new("any / opaque id\n").unwrap().as_str(),
        "any / opaque id\n"
    );
    assert_eq!(TurnId::new(0), Err(ValueError::ZeroTurnId));
    for name in ["read_file", "_read-2", "A"] {
        assert!(ToolName::new(name).is_ok());
    }
    for name in ["", "2read", "read.file", "read file", "读取"] {
        assert!(ToolName::new(name).is_err());
    }
}
