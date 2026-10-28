use super::text;
use cola::domain::{
    CallRef, GenerationId, NonEmptyText, SessionId, Timestamp, ToolCallId, ToolName, TurnId,
    ValueError,
};

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

#[test]
fn invocation_numbers_timestamps_and_scoped_original_ids_preserve_values() {
    assert_eq!(GenerationId::new(0), Err(ValueError::ZeroGenerationId));
    let generation = GenerationId::new(u64::MAX).unwrap();
    assert_eq!(generation.get(), u64::MAX);
    let id = ToolCallId::new(" 原始 / opaque\n ").unwrap();
    let call = CallRef::new(generation, id.clone());
    assert_eq!(call.generation(), generation);
    assert_eq!(call.call_id(), &id);
    assert_ne!(call, CallRef::new(GenerationId::new(1).unwrap(), id));
    assert_eq!(
        Timestamp::from_unix_millis(u64::MAX).unix_millis(),
        u64::MAX
    );
    assert_eq!(Timestamp::from_unix_millis(0).unix_millis(), 0);
}
