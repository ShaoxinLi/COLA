use super::text;
use cola::domain::Decision;

#[test]
fn decisions_preserve_allowance_and_optional_checked_reasons() {
    let allow = Decision::new(true, None);
    assert!(allow.should_allow());
    assert_eq!(allow.reason(), None);
    let reason = text("  secret files need explicit approval  ");
    let deny = Decision::new(false, Some(reason.clone()));
    assert!(!deny.should_allow());
    assert_eq!(deny.reason(), Some(&reason));
}
