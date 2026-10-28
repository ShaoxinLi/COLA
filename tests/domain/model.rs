use cola::domain::{ModelErrorKind, ModelId};

#[test]
fn model_ids_preserve_nonblank_values_and_redact_diagnostics() {
    let id = ModelId::new("  MODEL_SENTINEL  ").unwrap();
    assert_eq!(id.as_str(), "  MODEL_SENTINEL  ");
    assert!(!format!("{id:?}").contains("SENTINEL"));
    for value in ["", " ", "\n\t"] {
        assert_eq!(
            ModelId::new(value).unwrap_err().kind(),
            ModelErrorKind::InvalidSettings
        );
    }
}
