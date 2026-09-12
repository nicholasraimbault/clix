mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clix::{sync_after_pair, BodyId, Peer, Store};
use common::{paired, TestDaemon};
use serde_json::json;

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

#[tokio::test]
async fn pin_io_error_is_not_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.body_name = "laptop".into();
    store.pin_dir = Some(dir.path().join("src"));
    store.save().unwrap();
    let sk = store.owner_sk.clone();
    let store = Arc::new(Mutex::new(store));
    let peer = Peer {
        name: BodyId("server".into()),
        owner_pk: vec![0; 32],
        addr: Some("127.0.0.1:1".into()),
    };
    let err = sync_after_pair(&store, &sk, &peer).await.unwrap_err();
    assert!(
        !err.to_string().contains("Not merging"),
        "I/O must surface as an error, got {err}"
    );
}

#[tokio::test]
async fn unknown_peer_on_pair_is_ok() {
    let b = TestDaemon::spawn_named("server").await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.body_name = "laptop".into();
    store.pin_dir = Some(dir.path().join("src"));
    store.save().unwrap();
    let sk = store.owner_sk.clone();
    let store = Arc::new(Mutex::new(store));
    let peer = Peer {
        name: BodyId("server".into()),
        owner_pk: vec![1; 32],
        addr: Some(b.mesh_addr().to_string()),
    };
    sync_after_pair(&store, &sk, &peer)
        .await
        .expect("401 unknown peer is the documented pair-race skip");
}

#[tokio::test]
async fn conflict_blocks_mesh_exec() {
    let (laptop, server) = paired("laptop", "server").await;
    write_rel(&laptop.pin_dir(), "foo.rs", "laptop wrote this");
    write_rel(&server.pin_dir(), "foo.rs", "server wrote this");
    laptop
        .rpc(json!({"op": "add", "tool": "true"}))
        .await
        .unwrap();
    let err = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Not merging"), "{msg}");
}

#[tokio::test]
async fn start_conflict_does_not_run_waiting_exec() {
    std::env::set_var("CLIX_WAIT_POLL", "50ms");
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let script = dir.path().join("markonce");
    fs::write(
        &script,
        format!("#!/bin/sh\necho x >> '{}'\n", marker.display()),
    )
    .unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();

    let (laptop, server) = paired("laptop", "server").await;
    laptop
        .rpc(json!({"op": "add", "tool": script.to_str().unwrap()}))
        .await
        .unwrap();
    write_rel(&laptop.pin_dir(), "foo.rs", "laptop wrote this");
    write_rel(&server.pin_dir(), "foo.rs", "server wrote this");
    laptop.kill().await;
    let waiting = server
        .rpc(json!({
            "op": "exec",
            "body": "laptop",
            "argv": ["markonce"],
            "no_wait": true
        }))
        .await
        .unwrap();
    assert_eq!(waiting["status"], "waiting", "{waiting}");
    let _laptop = laptop.restart().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let ran = fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        ran.is_empty(),
        "start-sync conflict must not run waiting exec, got {ran:?}"
    );
}

#[tokio::test]
async fn invalid_pin_content_is_not_ok() {
    let (laptop, server) = paired("laptop", "server").await;
    let err = server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op": "pin_put", "path": "x.rs", "content": "zz"}),
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("invalid pin content") || msg.contains("pin"),
        "{msg}"
    );
}
