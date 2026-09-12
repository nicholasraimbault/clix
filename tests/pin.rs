use std::fs;
use std::path::Path;

use clix::Store;

fn write_rel(root: &Path, rel: &str, contents: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, contents).unwrap();
}

fn store_named(name: &str) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.body_name = name.to_string();
    store.save().unwrap();
    (dir, store)
}

#[test]
fn copies_new_file_to_peer() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write_rel(a.path(), "hello.rs", "fn main() {}");
    let (_state, mut store) = store_named("laptop");
    clix::pin_sync(&mut store, a.path(), b.path(), "server").unwrap();
    assert_eq!(
        fs::read_to_string(b.path().join("hello.rs")).unwrap(),
        "fn main() {}"
    );
}

#[test]
fn conflict_stops() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write_rel(a.path(), "foo.rs", "laptop wrote this");
    write_rel(b.path(), "foo.rs", "server wrote this");
    let (_state, mut store) = store_named("laptop");
    let err = clix::pin_sync(&mut store, a.path(), b.path(), "server").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Not merging"), "{msg}");
    assert!(msg.contains("foo.rs"), "{msg}");
    assert_eq!(
        fs::read_to_string(a.path().join("foo.rs")).unwrap(),
        "laptop wrote this"
    );
    assert_eq!(
        fs::read_to_string(b.path().join("foo.rs")).unwrap(),
        "server wrote this"
    );
}
