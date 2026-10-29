use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use std::{fs, process::Command};
#[test]
fn empty_document_roundtrip_and_stale_commit_invalidates_the_writer() {
    let root = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(root.path());
    assert!(store.list().unwrap().is_empty());
    let mut writer = store.create().unwrap();
    let id = writer.committed().session().id().clone();
    assert_eq!(writer.committed().revision(), 0);
    assert_eq!(store.inspect(&id).unwrap(), *writer.committed());
    assert_eq!(
        writer
            .commit(writer.committed().clone())
            .unwrap_err()
            .kind(),
        SessionStoreErrorKind::StaleRevision
    );
    assert_eq!(
        writer
            .commit(writer.committed().clone())
            .unwrap_err()
            .kind(),
        SessionStoreErrorKind::WriterInvalidated
    );
    assert_eq!(store.list().unwrap(), vec![id]);
}
#[test]
fn independent_process_lock_is_stable_and_released_on_exit() {
    let root = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(root.path());
    let writer = store.create().unwrap();
    let id = writer.committed().session().id().clone();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "jsonl::lock_child", "--nocapture"])
        .env("COLA_LOCK_ROOT", root.path())
        .env("COLA_LOCK_ID", id.as_str())
        .env("COLA_EXPECT_BUSY", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    drop(writer);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "jsonl::lock_child", "--nocapture"])
        .env("COLA_LOCK_ROOT", root.path())
        .env("COLA_LOCK_ID", id.as_str())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(store.acquire(&id).is_ok());
}
#[test]
fn lock_child() {
    let Some(root) = std::env::var_os("COLA_LOCK_ROOT") else {
        return;
    };
    let store = JsonlSessionStore::new(root);
    let id = SessionId::new(std::env::var("COLA_LOCK_ID").unwrap()).unwrap();
    if std::env::var_os("COLA_EXPECT_BUSY").is_some() {
        assert_eq!(
            store.acquire(&id).err().unwrap().kind(),
            SessionStoreErrorKind::Busy
        );
    } else {
        assert!(store.acquire(&id).is_ok());
    }
}
#[test]
fn old_version_corruption_and_identity_mismatch_are_preserved_and_refused() {
    let root = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(root.path());
    let writer = store.create().unwrap();
    let id = writer.committed().session().id().clone();
    drop(writer);
    let path = fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|s| s == "jsonl"))
        .unwrap();
    let original = fs::read(&path).unwrap();
    for (key, value, kind) in [
        (
            "version",
            serde_json::json!(1),
            SessionStoreErrorKind::UnsupportedVersion,
        ),
        (
            "session_id",
            serde_json::json!("wrong"),
            SessionStoreErrorKind::InvalidData,
        ),
        (
            "closed_turn_count",
            serde_json::json!(2),
            SessionStoreErrorKind::InvalidData,
        ),
    ] {
        let mut header: serde_json::Value = serde_json::from_slice(&original).unwrap();
        header[key] = value;
        let bytes = format!("{header}\n").into_bytes();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.inspect(&id).unwrap_err().kind(), kind);
        assert_eq!(store.acquire(&id).err().unwrap().kind(), kind);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::write(&path, b"{broken\n").unwrap();
    assert_eq!(
        store.list().unwrap_err().kind(),
        SessionStoreErrorKind::InvalidData
    );
}
