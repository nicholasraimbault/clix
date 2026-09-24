mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clix::{sync_after_pair, BodyId, Peer, Store};
use common::{paired, paired_pinned, TestDaemon};
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
    let (_la, mut laptop) = store_named("laptop");
    let (_sa, mut server) = store_named("server");
    clix::pin_sync(&mut laptop, a.path(), &mut server, b.path()).unwrap();
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
    let (_la, mut laptop) = store_named("laptop");
    let (_sa, mut server) = store_named("server");
    let err = clix::pin_sync(&mut laptop, a.path(), &mut server, b.path()).unwrap_err();
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

#[test]
fn one_side_edit_copies_after_sync() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    write_rel(a.path(), "foo.rs", "v1");
    let (_la, mut laptop) = store_named("laptop");
    let (_sa, mut server) = store_named("server");
    clix::pin_sync(&mut laptop, a.path(), &mut server, b.path()).unwrap();
    assert_eq!(fs::read_to_string(b.path().join("foo.rs")).unwrap(), "v1");
    assert!(laptop.pin_index.last_sync.is_some());
    assert!(
        server.pin_index.last_sync.is_some(),
        "both stores must record last successful sync"
    );
    std::thread::sleep(Duration::from_millis(5));
    write_rel(a.path(), "foo.rs", "v2");
    clix::pin_sync(&mut server, b.path(), &mut laptop, a.path()).unwrap();
    assert_eq!(fs::read_to_string(b.path().join("foo.rs")).unwrap(), "v2");
    assert_eq!(fs::read_to_string(a.path().join("foo.rs")).unwrap(), "v2");
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
async fn unknown_peer_on_pair_is_denied() {
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
        .expect_err("unknown identities must never count as a successful sync");
}

#[tokio::test]
async fn mesh_one_side_edit_copies() {
    let (laptop, server) = paired_pinned("laptop", "server").await;
    write_rel(&laptop.pin_dir(), "foo.rs", "v1");
    server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(server.pin_dir().join("foo.rs")).unwrap(),
        "v1"
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    write_rel(&laptop.pin_dir(), "foo.rs", "v2");
    let synced = server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await
        .unwrap();
    assert_eq!(synced["synced"], true, "one-sided edit must copy: {synced}");
    assert_eq!(
        fs::read_to_string(server.pin_dir().join("foo.rs")).unwrap(),
        "v2"
    );
}

#[tokio::test]
async fn waiting_exec_runs_when_the_peer_returns_regardless_of_pins() {
    // Exec is decoupled from pin sync: a waiting job runs when the peer comes
    // back even if the pin trees conflict. Pin honesty is a separate concern.
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
    tokio::time::sleep(Duration::from_millis(600)).await;
    let ran = fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        ran, "x\n",
        "waiting exec should run when the peer returns, got {ran:?}"
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

#[test]
fn preserved_timestamps_do_not_hide_both_sides_edits() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (_ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    write_rel(a.path(), "file", "base");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    write_rel(a.path(), "file", "local edit");
    write_rel(b.path(), "file", "remote edit");
    fs::File::open(a.path().join("file"))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    assert!(clix::pin_sync(&mut sa, a.path(), &mut sb, b.path())
        .unwrap_err()
        .to_string()
        .contains("Not merging"));
    assert_eq!(
        fs::read_to_string(a.path().join("file")).unwrap(),
        "local edit"
    );
    assert_eq!(
        fs::read_to_string(b.path().join("file")).unwrap(),
        "remote edit"
    );
}

#[test]
fn deletions_propagate_and_displaced_contents_are_recoverable() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (_ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    write_rel(a.path(), "file", "keep a recoverable version");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    fs::remove_file(a.path().join("file")).unwrap();
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    assert!(!b.path().join("file").exists());
    let recovery = fs::read_dir(b.path().join(".clix-recovery"))
        .unwrap()
        .map(|e| fs::read(e.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert!(recovery.contains(&b"keep a recoverable version".to_vec()));
    clix::pin_sync(&mut sb, b.path(), &mut sa, a.path()).unwrap();
    assert!(!a.path().join("file").exists());
}

#[test]
fn deletion_against_edit_is_a_conflict() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (_ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    write_rel(a.path(), "file", "base");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    fs::remove_file(a.path().join("file")).unwrap();
    write_rel(b.path(), "file", "edited");
    assert!(clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).is_err());
    assert_eq!(fs::read_to_string(b.path().join("file")).unwrap(), "edited");
}

#[test]
fn a_third_peer_does_not_replace_the_first_peers_baseline() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let (_ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    let (_cd, mut sc) = store_named("phone");
    write_rel(a.path(), "file", "base");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    write_rel(a.path(), "file", "local edit");
    clix::pin_sync(&mut sa, a.path(), &mut sc, c.path()).unwrap();
    write_rel(b.path(), "file", "server edit");
    assert!(clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).is_err());
}

#[tokio::test]
async fn network_pin_preserves_executable_mode_and_binary_content() {
    let (laptop, server) = paired_pinned("laptop", "server").await;
    fs::write(laptop.pin_dir().join("binary"), [0, 255, 128]).unwrap();
    fs::set_permissions(
        laptop.pin_dir().join("binary"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    server
        .rpc(json!({"op":"pin_sync","body":"laptop"}))
        .await
        .unwrap();
    assert_eq!(
        fs::read(server.pin_dir().join("binary")).unwrap(),
        vec![0, 255, 128]
    );
    assert_eq!(
        fs::metadata(server.pin_dir().join("binary"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
}

#[test]
fn pending_recovery_receipt_blocks_sync_after_restart() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    write_rel(a.path(), "file", "base");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    write_rel(
        a.path(),
        ".clix-recovery/interrupted.json",
        r#"{"path":"file","state":"pending"}"#,
    );
    sa = Store::open(ad.path()).unwrap();
    let error = clix::pin_sync(&mut sa, a.path(), &mut sb, b.path())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("file") && error.contains("interrupted.json"),
        "{error}"
    );
}

#[test]
fn late_write_through_retained_inode_is_reported_and_preserved() {
    use std::io::{Seek, SeekFrom, Write};
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (_ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    write_rel(a.path(), "file", "base");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    let mut owner = fs::OpenOptions::new()
        .write(true)
        .open(b.path().join("file"))
        .unwrap();
    write_rel(a.path(), "file", "new content");
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    owner.seek(SeekFrom::Start(0)).unwrap();
    owner.write_all(b"late owner edit").unwrap();
    owner.sync_all().unwrap();
    let error = clix::pin_sync(&mut sa, a.path(), &mut sb, b.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("retained version changed"), "{error}");
    assert!(fs::read_dir(b.path().join(".clix-recovery"))
        .unwrap()
        .any(|e| fs::read(e.unwrap().path()).unwrap() == b"late owner edit"));
}

#[test]
fn pin_cannot_replace_an_active_granted_binary() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (_ad, mut sa) = store_named("laptop");
    let (_bd, mut sb) = store_named("server");
    write_rel(a.path(), "tool", "original");
    fs::set_permissions(a.path().join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
    clix::pin_sync(&mut sa, a.path(), &mut sb, b.path()).unwrap();
    clix::add(
        &mut sb,
        b.path().join("tool").to_str().unwrap(),
        &[],
        false,
        None,
        None,
    )
    .unwrap();
    write_rel(a.path(), "tool", "remote replacement");
    let error = clix::pin_sync(&mut sa, a.path(), &mut sb, b.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("granted binary"), "{error}");
    assert_eq!(fs::read(b.path().join("tool")).unwrap(), b"original");
}

#[tokio::test]
async fn mesh_exec_runs_despite_a_pin_conflict() {
    // Remote execution is no longer gated on a pin sync: a real ~/src can
    // exceed the pin scan limits or hold a conflict without breaking exec.
    // Pin honesty is still enforced by an explicit sync (below).
    let (laptop, server) = paired_pinned("laptop", "server").await;
    write_rel(&laptop.pin_dir(), "foo.rs", "laptop wrote this");
    write_rel(&server.pin_dir(), "foo.rs", "server wrote this");
    laptop
        .rpc(json!({"op": "add", "tool": "true"}))
        .await
        .unwrap();
    let result = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(result["status"], "done", "{result}");
    assert_eq!(result["exit"], 0, "{result}");
    // An explicit pin sync still refuses to merge the conflict.
    let synced = server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await
        .unwrap();
    assert_eq!(synced["synced"], false, "{synced}");
    assert!(
        synced["error"].as_str().unwrap().contains("Not merging"),
        "{synced}"
    );
}

#[tokio::test]
async fn receiver_rejects_a_pin_put_that_diverges_from_its_baseline() {
    // The honest-conflict rule must be enforced on the receiver, not only by the
    // initiating planner. A paired peer that reads the receiver's current
    // fingerprint (pin_list) and writes with expected=current must not be able
    // to overwrite a local edit the receiver made since the last sync.
    let (laptop, server) = paired_pinned("laptop", "server").await;
    // Establish a shared baseline for notes.txt.
    write_rel(&laptop.pin_dir(), "notes.txt", "v1\n");
    server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await
        .unwrap();
    // The laptop owner edits locally and does NOT sync: it has diverged from
    // the shared baseline.
    write_rel(&laptop.pin_dir(), "notes.txt", "laptop local edit\n");
    // The server reads the laptop's current fingerprint and tries to overwrite
    // it directly, supplying expected = the laptop's current version.
    let listing = server
        .mesh_raw(laptop.mesh_addr(), json!({"op": "pin_list"}))
        .await
        .unwrap();
    let current = listing["files"]["notes.txt"].clone();
    assert!(current.is_object(), "listing: {listing}");
    let overwrite = fs::read("/usr/bin/true").ok();
    let _ = overwrite;
    let put = server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({
                "op": "pin_put",
                "path": "notes.txt",
                "expected": current,
                "new": {"hash": sha256_hex(b"server overwrite\n"), "mode": 420},
                "content": hex_of(b"server overwrite\n"),
            }),
        )
        .await;
    assert!(
        put.is_err(),
        "receiver must refuse the diverging write: {put:?}"
    );
    // The laptop's local edit is intact.
    assert_eq!(
        fs::read_to_string(laptop.pin_dir().join("notes.txt")).unwrap(),
        "laptop local edit\n"
    );
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn pin_excludes_git_hooks_from_sync_and_incoming_writes() {
    let (laptop, server) = paired_pinned("laptop", "server").await;
    // An existing hook on the laptop is not replicated by a sync.
    write_rel(
        &laptop.pin_dir(),
        ".git/hooks/pre-commit",
        "#!/bin/sh\ntrue\n",
    );
    write_rel(&laptop.pin_dir(), "keep.txt", "ok\n");
    server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(server.pin_dir().join("keep.txt")).unwrap(),
        "ok\n"
    );
    assert!(
        !server.pin_dir().join(".git/hooks/pre-commit").exists(),
        "git hooks must not replicate through pin"
    );
    // A peer cannot plant a hook by writing directly to a .git/hooks path.
    let put = server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({
                "op": "pin_put",
                "path": ".git/hooks/pre-commit",
                "expected": null,
                "new": {"hash": sha256_hex(b"evil\n"), "mode": 493},
                "content": hex_of(b"evil\n"),
            }),
        )
        .await;
    assert!(put.is_err(), "planting a git hook must be refused: {put:?}");
    // The owner's own hook is untouched; the peer's content was not written.
    assert_eq!(
        fs::read_to_string(laptop.pin_dir().join(".git/hooks/pre-commit")).unwrap(),
        "#!/bin/sh\ntrue\n"
    );
}

#[tokio::test]
async fn pin_is_opt_in_and_off_by_default() {
    let laptop = TestDaemon::spawn_named("laptop").await;
    let server = TestDaemon::spawn_named("server").await;
    // A file exists before pairing; with pin off it must not be copied.
    write_rel(&laptop.pin_dir(), "early.txt", "before pairing\n");
    let phrase = laptop.rpc(json!({"op": "pair_start"})).await.unwrap()["phrase"]
        .as_str()
        .unwrap()
        .to_string();
    server
        .rpc(json!({"op": "pair_join", "phrase": phrase}))
        .await
        .unwrap();
    assert!(
        !server.pin_dir().join("early.txt").exists(),
        "pairing must not sync ~/src while pin is off"
    );
    // An explicit sync is refused while pin is off.
    let off = server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await;
    assert!(
        off.is_err(),
        "sync must be refused while pin is off: {off:?}"
    );
    // A peer cannot read the manifest of a machine with pin off.
    let listing = server
        .mesh_raw(laptop.mesh_addr(), json!({"op": "pin_list"}))
        .await;
    assert!(
        listing.is_err(),
        "pin_list must be refused while pin is off"
    );
    // Opting in on both machines enables sync.
    laptop.rpc(json!({"op": "pin_on"})).await.unwrap();
    server.rpc(json!({"op": "pin_on"})).await.unwrap();
    let synced = server
        .rpc(json!({"op": "pin_sync", "body": "laptop"}))
        .await
        .unwrap();
    assert_eq!(synced["synced"], true, "{synced}");
    assert_eq!(
        fs::read_to_string(server.pin_dir().join("early.txt")).unwrap(),
        "before pairing\n"
    );
}

#[tokio::test]
async fn status_reports_pin_state() {
    let daemon = TestDaemon::spawn_named("laptop").await;
    let status = daemon.rpc(json!({"op": "status"})).await.unwrap();
    assert_eq!(
        status["pin_enabled"], false,
        "pin is off by default: {status}"
    );
    daemon.rpc(json!({"op": "pin_on"})).await.unwrap();
    let status = daemon.rpc(json!({"op": "status"})).await.unwrap();
    assert_eq!(status["pin_enabled"], true, "{status}");
}
