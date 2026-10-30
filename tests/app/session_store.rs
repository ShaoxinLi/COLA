use cola::app::SessionStoreErrorKind;

#[test]
fn port_errors_preserve_the_original_source() {
    use cola::app::SessionStoreError;
    use std::{error::Error, io};
    let error = SessionStoreError::new(
        SessionStoreErrorKind::Storage,
        io::Error::new(io::ErrorKind::PermissionDenied, "original"),
    );
    let source = error.source().unwrap().downcast_ref::<io::Error>().unwrap();
    assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(source.to_string(), "original");
}
