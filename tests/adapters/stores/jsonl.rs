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

#[test]
fn missing_ids_do_not_create_store_or_lock_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("absent-store");
    let store = JsonlSessionStore::new(&path);
    let missing = SessionId::new("missing-web-id").unwrap();
    assert_eq!(
        store.acquire(&missing).err().unwrap().kind(),
        SessionStoreErrorKind::NotFound
    );
    assert!(!path.exists());
    fs::create_dir(&path).unwrap();
    assert_eq!(
        store.acquire(&missing).err().unwrap().kind(),
        SessionStoreErrorKind::NotFound
    );
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
}
#[test]
fn copied_authoritative_archive_recreates_a_stable_lock() {
    let source = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(source.path());
    let writer = store.create().unwrap();
    let document = writer.committed().clone();
    drop(writer);
    let archive = fs::read_dir(source.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .unwrap();
    let destination = tempfile::tempdir().unwrap();
    fs::copy(
        &archive,
        destination.path().join(archive.file_name().unwrap()),
    )
    .unwrap();
    let copied = JsonlSessionStore::new(destination.path());
    let writer = copied.acquire(document.session().id()).unwrap();
    assert_eq!(writer.committed(), &document);
    assert_eq!(
        copied
            .acquire(document.session().id())
            .err()
            .unwrap()
            .kind(),
        SessionStoreErrorKind::Busy
    );
    drop(writer);
    assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 2);
    assert_eq!(
        copied.acquire(document.session().id()).unwrap().committed(),
        &document
    );
}
