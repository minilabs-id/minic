//! M3: persistensi sesi + prefix-compare.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minic::store::SessionStore;
use minic_core::types::Message;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("minic_m3_{tag}_{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn sample() -> Vec<Message> {
    vec![
        Message::user("hello"),
        Message::assistant("hi there"),
        Message::user("second"),
    ]
}

#[test]
fn roundtrip_is_lossless() {
    let root = temp_root("roundtrip");
    let mut store = SessionStore::open(&root).unwrap();
    store
        .upsert_session("s1", "m", &root.to_string_lossy())
        .unwrap();

    let messages = sample();
    let written = store.save_messages("s1", &messages).unwrap();
    assert_eq!(written, 3);

    let loaded = store.load_messages("s1").unwrap();
    assert_eq!(loaded, messages);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn append_only_writes_new_tail() {
    let root = temp_root("append");
    let mut store = SessionStore::open(&root).unwrap();
    store.upsert_session("s1", "m", ".").unwrap();

    store.save_messages("s1", &sample()).unwrap();

    let mut extended = sample();
    extended.push(Message::assistant("third"));
    let written = store.save_messages("s1", &extended).unwrap();
    assert_eq!(written, 1, "hanya 1 pesan baru ditulis");
    assert_eq!(store.load_messages("s1").unwrap(), extended);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shrink_drops_tail() {
    let root = temp_root("shrink");
    let mut store = SessionStore::open(&root).unwrap();
    store.upsert_session("s1", "m", ".").unwrap();

    store.save_messages("s1", &sample()).unwrap();
    let shrunk: Vec<Message> = sample().into_iter().take(2).collect();
    let written = store.save_messages("s1", &shrunk).unwrap();
    assert_eq!(written, 0);
    assert_eq!(store.load_messages("s1").unwrap(), shrunk);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn divergence_rewrites_from_split_point() {
    let root = temp_root("diverge");
    let mut store = SessionStore::open(&root).unwrap();
    store.upsert_session("s1", "m", ".").unwrap();

    store.save_messages("s1", &sample()).unwrap();
    let changed = vec![Message::user("hello"), Message::assistant("DIFFERENT")];
    store.save_messages("s1", &changed).unwrap();
    assert_eq!(store.load_messages("s1").unwrap(), changed);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn latest_session_is_most_recent() {
    let root = temp_root("latest");
    let store = SessionStore::open(&root).unwrap();
    store.upsert_session("a", "m", ".").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    store.upsert_session("b", "m", ".").unwrap();

    assert_eq!(store.latest_session().unwrap(), Some("b".to_string()));
    assert_eq!(store.list_sessions().unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(&root);
}
