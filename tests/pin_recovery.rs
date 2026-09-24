use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clix::{
    recovery_discard, recovery_inspect, recovery_list, recovery_read, recovery_resolve, BodyId,
    RecoveryChoice, RecoveryRead, RecoveryReceipt, RecoveryState, Store,
};
use serde_json::json;
use sha2::{Digest, Sha256};

struct Fixture {
    _dir: tempfile::TempDir,
    a: Store,
    b: Store,
    ap: PathBuf,
    bp: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ap = dir.path().join("a-pin");
        let bp = dir.path().join("b-pin");
        fs::create_dir(&ap).unwrap();
        fs::create_dir(&bp).unwrap();
        let mut a = Store::open(&dir.path().join("a-state")).unwrap();
        let mut b = Store::open(&dir.path().join("b-state")).unwrap();
        a.body_name = "a".into();
        b.body_name = "b".into();
        a.pin_dir = Some(ap.clone());
        b.pin_dir = Some(bp.clone());
        Self {
            _dir: dir,
            a,
            b,
            ap,
            bp,
        }
    }
    fn sync(&mut self) {
        clix::pin_sync(&mut self.a, &self.ap, &mut self.b, &self.bp).unwrap();
    }
    fn replaced() -> (Self, String) {
        let mut f = Self::new();
        fs::write(f.ap.join("file"), b"original\0\xff").unwrap();
        fs::set_permissions(f.ap.join("file"), fs::Permissions::from_mode(0o751)).unwrap();
        f.sync();
        fs::write(f.ap.join("file"), b"replacement").unwrap();
        f.sync();
        let id = recovery_list(&f.b)
            .unwrap()
            .entries
            .into_iter()
            .find(|r| r.state == "retained")
            .unwrap()
            .id;
        (f, id)
    }
}
fn receipt_path(root: &Path, id: &str) -> PathBuf {
    root.join(".clix-recovery").join(id)
}
fn write_receipt(root: &Path, id: &str, r: &RecoveryReceipt) {
    fs::write(receipt_path(root, id), serde_json::to_vec(r).unwrap()).unwrap();
}

#[test]
fn restore_is_version_checked_and_rejournals_the_displaced_live_version() {
    let (f, id) = Fixture::replaced();
    let before = recovery_inspect(&f.b, &id).unwrap();
    assert_eq!(
        recovery_read(&f.b, &id, RecoveryRead::Retained, &before.token).unwrap(),
        b"original\0\xff"
    );
    assert_eq!(
        recovery_read(&f.b, &id, RecoveryRead::Live, &before.token).unwrap(),
        b"replacement"
    );
    let after =
        recovery_resolve(&f.b, &id, &before.token, RecoveryChoice::RestoreRetained).unwrap();
    assert_eq!(after.receipt.state, RecoveryState::Resolved);
    assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"original\0\xff");
    assert_eq!(
        fs::metadata(f.bp.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    let mut preserved = false;
    for row in recovery_list(&f.b).unwrap().entries {
        let v = recovery_inspect(&f.b, &row.id).unwrap();
        if v.retained.is_some()
            && recovery_read(&f.b, &row.id, RecoveryRead::Retained, &v.token).unwrap()
                == b"replacement"
        {
            preserved = true;
        }
    }
    assert!(
        preserved,
        "restore must retain displaced live contents through the same publication journal"
    );
    assert!(recovery_resolve(&f.b, &id, &before.token, RecoveryChoice::RestoreRetained).is_err());
}

#[test]
fn stale_live_or_retained_versions_cannot_be_resolved_or_discarded() {
    let (f, id) = Fixture::replaced();
    let before = recovery_inspect(&f.b, &id).unwrap();
    fs::write(f.bp.join("file"), b"new owner edit").unwrap();
    assert!(recovery_resolve(&f.b, &id, &before.token, RecoveryChoice::RestoreRetained).is_err());
    assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"new owner edit");
    let current = recovery_inspect(&f.b, &id).unwrap();
    fs::write(
        receipt_path(&f.bp, &current.receipt.version),
        b"late retained edit",
    )
    .unwrap();
    assert!(recovery_discard(&f.b, &id, &current.token).is_err());
    assert!(receipt_path(&f.bp, &id).is_file());
    assert_eq!(
        fs::read(receipt_path(&f.bp, &current.receipt.version)).unwrap(),
        b"late retained edit"
    );
}

#[test]
fn keep_live_acknowledges_review_but_future_retained_writes_block_again() {
    let (mut f, id) = Fixture::replaced();
    let before = recovery_inspect(&f.b, &id).unwrap();
    fs::write(
        receipt_path(&f.bp, &before.receipt.version),
        b"late owner edit",
    )
    .unwrap();
    assert!(recovery_list(&f.b).unwrap().blocked);
    let reviewed = recovery_inspect(&f.b, &id).unwrap();
    recovery_resolve(&f.b, &id, &reviewed.token, RecoveryChoice::KeepLive).unwrap();
    assert!(!recovery_list(&f.b).unwrap().blocked);
    f.sync();
    fs::write(
        receipt_path(&f.bp, &before.receipt.version),
        b"another late edit",
    )
    .unwrap();
    assert!(recovery_list(&f.b).unwrap().blocked);
    assert!(clix::pin_sync(&mut f.a, &f.ap, &mut f.b, &f.bp).is_err());
    assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"replacement");
}

#[test]
fn pending_post_publication_receipt_requires_explicit_review_after_reopen() {
    let (mut f, id) = Fixture::replaced();
    let view = recovery_inspect(&f.b, &id).unwrap();
    let mut interrupted = view.receipt;
    interrupted.state = RecoveryState::Pending;
    write_receipt(&f.bp, &id, &interrupted);
    f.b = Store::open(&f._dir.path().join("b-state")).unwrap();
    f.b.pin_dir = Some(f.bp.clone());
    let current = recovery_inspect(&f.b, &id).unwrap();
    assert!(current.explanation.contains("appears applied"));
    assert!(recovery_list(&f.b).unwrap().blocked);
    assert!(recovery_discard(&f.b, &id, &current.token).is_err());
    recovery_resolve(&f.b, &id, &current.token, RecoveryChoice::KeepLive).unwrap();
    f.sync();
}

#[test]
fn staged_creation_is_inspectable_and_restorable_without_losing_staged_data() {
    let f = Fixture::new();
    let recovery = f.bp.join(".clix-recovery");
    fs::create_dir(&recovery).unwrap();
    fs::write(recovery.join("creation"), b"staged bytes").unwrap();
    fs::set_permissions(recovery.join("creation"), fs::Permissions::from_mode(0o640)).unwrap();
    let hash = Sha256::digest(b"staged bytes")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    fs::write(recovery.join("creation.json"),serde_json::to_vec(&json!({"path":"new","expected":null,"replacement":{"hash":hash,"mode":0o640},"version":"creation","state":"pending"})).unwrap()).unwrap();
    let view = recovery_inspect(&f.b, "creation.json").unwrap();
    assert!(view.explanation.contains("staged"));
    recovery_resolve(
        &f.b,
        "creation.json",
        &view.token,
        RecoveryChoice::RestoreRetained,
    )
    .unwrap();
    assert_eq!(fs::read(f.bp.join("new")).unwrap(), b"staged bytes");
    assert_eq!(
        fs::read(recovery.join("creation")).unwrap(),
        b"staged bytes"
    );
    assert!(!recovery_list(&f.b).unwrap().blocked);
}

#[test]
fn completed_creation_does_not_retain_empty_journal_metadata() {
    let mut f = Fixture::new();
    fs::write(f.ap.join("new"), b"new").unwrap();
    f.sync();
    assert!(recovery_list(&f.b).unwrap().entries.is_empty());
    assert_eq!(fs::read(f.bp.join("new")).unwrap(), b"new");
}

#[test]
fn interrupted_discard_finishes_without_touching_live_contents() {
    let (f, id) = Fixture::replaced();
    let view = recovery_inspect(&f.b, &id).unwrap();
    let mut interrupted = view.receipt.clone();
    interrupted.state = RecoveryState::Discarding;
    interrupted.discard_token = Some(view.token.clone());
    write_receipt(&f.bp, &id, &interrupted);
    fs::remove_file(receipt_path(&f.bp, &interrupted.version)).unwrap();
    assert!(recovery_list(&f.b).unwrap().blocked);
    recovery_discard(&f.b, &id, &view.token).unwrap();
    assert!(!receipt_path(&f.bp, &id).exists());
    assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"replacement");
}

#[test]
fn orphan_inspection_never_guesses_a_path_and_disposal_is_explicit() {
    let mut f = Fixture::new();
    let recovery = f.bp.join(".clix-recovery");
    fs::create_dir(&recovery).unwrap();
    fs::write(recovery.join("abandoned.tmp"), b"preserve me").unwrap();
    let list = recovery_list(&f.b).unwrap();
    assert!(list.blocked);
    assert_eq!(list.entries[0].state, "unreferenced");
    let view = recovery_inspect(&f.b, "abandoned.tmp").unwrap();
    assert!(!view.recorded);
    assert_eq!(
        recovery_read(&f.b, "abandoned.tmp", RecoveryRead::Retained, &view.token).unwrap(),
        b"preserve me"
    );
    assert!(recovery_resolve(
        &f.b,
        "abandoned.tmp",
        &view.token,
        RecoveryChoice::RestoreRetained
    )
    .is_err());
    assert!(clix::pin_sync(&mut f.a, &f.ap, &mut f.b, &f.bp).is_err());
    recovery_discard(&f.b, "abandoned.tmp", &view.token).unwrap();
    assert!(!recovery_list(&f.b).unwrap().blocked);
    f.sync();
}

#[test]
fn budget_exhaustion_preserves_existing_versions_and_refuses_publication() {
    let mut f = Fixture::new();
    let recovery = f.bp.join(".clix-recovery");
    fs::create_dir(&recovery).unwrap();
    let retained = fs::File::create(recovery.join("external-retained")).unwrap();
    retained.set_len(clix::MAX_RECOVERY_BYTES + 1).unwrap(); // sparse isolated file, not disk filling
    fs::write(f.ap.join("new"), b"new").unwrap();
    let report = recovery_list(&f.b).unwrap();
    assert!(report.usage.admission_blocked);
    assert!(report.blocked);
    assert!(clix::pin_sync(&mut f.a, &f.ap, &mut f.b, &f.bp).is_err());
    assert!(!f.bp.join("new").exists());
    assert_eq!(
        retained.metadata().unwrap().len(),
        clix::MAX_RECOVERY_BYTES + 1
    );
}

#[test]
fn recovery_restoration_cannot_replace_a_granted_binary_or_follow_symlinks() {
    let (mut f, id) = Fixture::replaced();
    let path = f.bp.join("file");
    clix::add(&mut f.b, path.to_str().unwrap(), &[], false, None, None).unwrap();
    let view = recovery_inspect(&f.b, &id).unwrap();
    assert!(
        recovery_resolve(&f.b, &id, &view.token, RecoveryChoice::RestoreRetained)
            .unwrap_err()
            .to_string()
            .contains("granted binary")
    );
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
    let saved = receipt_path(&f.bp, &view.receipt.version);
    fs::remove_file(&saved).unwrap();
    let outside = f._dir.path().join("outside");
    fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, &saved).unwrap();
    assert!(recovery_inspect(&f.b, &id).is_err());
    assert!(recovery_discard(&f.b, &id, &view.token).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"outside");
    assert!(recovery_inspect(&f.b, "../outside").is_err());
}

struct Network {
    _dir: tempfile::TempDir,
    a: Arc<Mutex<Store>>,
    b: Arc<Mutex<Store>>,
    ap: PathBuf,
    bp: PathBuf,
    tasks: Vec<tokio::task::JoinHandle<Result<(), clix::ClixError>>>,
}
impl Drop for Network {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
async fn network() -> Network {
    std::env::set_var("CLIX_NOTIFY", "0");
    std::env::set_var("CLIX_TRAY", "0");
    let dir = tempfile::tempdir().unwrap();
    let ap = dir.path().join("a-pin");
    let bp = dir.path().join("b-pin");
    fs::create_dir(&ap).unwrap();
    fs::create_dir(&bp).unwrap();
    let mut a = Store::open(&dir.path().join("a")).unwrap();
    let mut b = Store::open(&dir.path().join("b")).unwrap();
    a.body_name = "alpha".into();
    b.body_name = "beta".into();
    a.pin_dir = Some(ap.clone());
    b.pin_dir = Some(bp.clone());
    // Pin is opt-in; this fixture exercises pin recovery, so both opt in.
    a.pin_enabled = true;
    b.pin_enabled = true;
    let am = clix::MeshListener::bind("127.0.0.1:0").await.unwrap();
    let bm = clix::MeshListener::bind("127.0.0.1:0").await.unwrap();
    let apk = ed25519_dalek::SigningKey::from_bytes(a.owner_sk.as_slice().try_into().unwrap())
        .verifying_key()
        .to_bytes()
        .to_vec();
    let bpk = ed25519_dalek::SigningKey::from_bytes(b.owner_sk.as_slice().try_into().unwrap())
        .verifying_key()
        .to_bytes()
        .to_vec();
    // Synthetic authenticated peer fixtures, not a replacement for PAKE tests.
    a.peers.push(clix::Peer {
        name: BodyId("beta".into()),
        owner_pk: bpk,
        addr: Some(bm.local_addr().to_string()),
    });
    b.peers.push(clix::Peer {
        name: BodyId("alpha".into()),
        owner_pk: apk,
        addr: Some(am.local_addr().to_string()),
    });
    a.save().unwrap();
    b.save().unwrap();
    let a = Arc::new(Mutex::new(a));
    let b = Arc::new(Mutex::new(b));
    let asock = dir.path().join("a.sock");
    let bsock = dir.path().join("b.sock");
    let tasks = vec![
        tokio::spawn(clix::serve(a.clone(), asock.clone(), am)),
        tokio::spawn(clix::serve(b.clone(), bsock.clone(), bm)),
    ];
    for _ in 0..100 {
        if asock.exists() && bsock.exists() {
            return Network {
                _dir: dir,
                a,
                b,
                ap,
                bp,
                tasks,
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("owner listeners did not start")
}
#[tokio::test]
async fn ordinary_conflict_accepts_only_the_inspected_peer_version_locally() {
    let n = network().await;
    fs::write(n.ap.join("file"), b"local").unwrap();
    fs::write(n.bp.join("file"), b"peer").unwrap();
    let report = clix::inspect_peer_conflicts(&n.a, "beta").await.unwrap();
    assert_eq!(report.conflicts.len(), 1);
    let old = report.conflicts[0].clone();
    fs::write(n.bp.join("file"), b"new peer edit").unwrap();
    assert!(clix::take_peer_conflict(&n.a, &old).await.is_err());
    assert_eq!(fs::read(n.ap.join("file")).unwrap(), b"local");
    let selected = clix::inspect_peer_conflicts(&n.a, "beta")
        .await
        .unwrap()
        .conflicts
        .remove(0);
    clix::take_peer_conflict(&n.a, &selected).await.unwrap();
    assert_eq!(fs::read(n.ap.join("file")).unwrap(), b"new peer edit");
    assert_eq!(fs::read(n.bp.join("file")).unwrap(), b"new peer edit");
    assert!(clix::owner_pin_sync(&n.a, "beta").await.unwrap().synced);
    let store = n.a.lock().unwrap();
    assert!(recovery_list(&store)
        .unwrap()
        .entries
        .iter()
        .any(|row| row.state == "retained"));
}
#[tokio::test]
async fn accepting_peer_deletion_retains_local_edit_and_owner_operations_stay_off_mesh() {
    let n = network().await;
    fs::write(n.ap.join("file"), b"base").unwrap();
    assert!(clix::owner_pin_sync(&n.a, "beta").await.unwrap().synced);
    fs::write(n.ap.join("file"), b"local edit").unwrap();
    fs::remove_file(n.bp.join("file")).unwrap();
    let selected = clix::inspect_peer_conflicts(&n.a, "beta")
        .await
        .unwrap()
        .conflicts
        .remove(0);
    assert!(selected.remote.is_none());
    clix::take_peer_conflict(&n.a, &selected).await.unwrap();
    assert!(!n.ap.join("file").exists());
    assert!(!n.bp.join("file").exists());
    let (peer, sk) = {
        let b = n.b.lock().unwrap();
        (b.peers[0].clone(), b.owner_sk.clone())
    };
    for op in [
        "pin_recovery_resolve",
        "pin_recovery_discard",
        "pin_take_peer",
    ] {
        let error = clix::mesh_call(
            peer.addr.as_deref().unwrap(),
            &sk,
            &peer.owner_pk,
            json!({"op":op,"id":"anything"}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not allowed"));
    }
}

#[test]
fn real_process_death_during_restore_and_discard_preserves_recovery() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let harness = tempfile::tempdir().unwrap();
    let c = harness.path().join("crash.c");
    let so = harness.path().join("crash.so");
    fs::write(
        &c,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int fsync(int fd) {
    static int (*next)(int); static unsigned count;
    if (!next) next=dlsym(RTLD_NEXT,"fsync");
    int result=next(fd);
    char link[64],path[PATH_MAX];const char *root=getenv("CLIX_CRASH_RECOVERY_ROOT");
    snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
    ssize_t n=readlink(link,path,sizeof(path)-1);
    if(result==0 && root && n>=0) {
        path[n]=0;
        size_t len=strlen(root);
        if(!strncmp(path,root,len) && (path[len]==0 || path[len]=='/')) {
            const char *limit=getenv("CLIX_CRASH_FSYNC_NUMBER");
            if(limit && ++count==strtoul(limit,0,10)) kill(getpid(),SIGKILL);
        }
    }
    return result;
}
"#,
    )
    .unwrap();
    let compile = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c)
        .arg("-ldl")
        .arg("-o")
        .arg(&so)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let source = harness.path().join("driver.rs");
    let binary = harness.path().join("driver");
    fs::write(
        &source,
        r#"
fn main()->Result<(),Box<dyn std::error::Error>> {
    let args:Vec<_>=std::env::args().collect();
    let mut store=clix::Store::open(std::path::Path::new(&args[1]))?;
    store.pin_dir=Some(args[2].clone().into());
    if args[5]=="fork-lock" {
        unsafe extern "C" { fn fork()->i32; fn pause()->i32; fn kill(pid:i32, signal:i32)->i32; fn waitpid(pid:i32,status:*mut i32,options:i32)->i32; fn _exit(status:i32)->!; }
        let context=clix::RecoveryContext::from_store(&store);
        let export=context.export(&args[3],clix::RecoveryRead::Retained,&args[4])?;
        let child=unsafe {fork()};
        if child<0 {return Err("fork failed".into());}
        if child==0 {unsafe {pause();_exit(0);}}
        drop(export);
        let result=context.list_page(None,128);
        unsafe {kill(child,9);waitpid(child,std::ptr::null_mut(),0);}
        result?;
        return Ok(());
    }
    if args[5]=="restore" {
        clix::recovery_resolve(&store,&args[3],&args[4],clix::RecoveryChoice::RestoreRetained)?;
    } else {clix::recovery_discard(&store,&args[3],&args[4])?;}
    Ok(())
}
"#,
    )
    .unwrap();
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let library = fs::read_dir(&deps)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with("libclix-")
                && e.path().extension().is_some_and(|s| s == "rlib")
        })
        .max_by_key(|e| e.metadata().unwrap().modified().unwrap())
        .expect("built clix library")
        .path();
    let compile = Command::new("rustc")
        .arg("--edition=2021")
        .arg(&source)
        .arg("--extern")
        .arg(format!("clix={}", library.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = |f: &Fixture, id: &str, token: &str, mode: &str, number: u32| {
        let mut child = Command::new(&binary)
            .arg(f._dir.path().join("b-state"))
            .arg(&f.bp)
            .arg(id)
            .arg(token)
            .arg(mode)
            .env("LD_PRELOAD", &so)
            .env("CLIX_CRASH_RECOVERY_ROOT", f.bp.join(".clix-recovery"))
            .env("CLIX_CRASH_FSYNC_NUMBER", number.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(
                    status.signal(),
                    Some(9),
                    "{mode} at fsync {number}: {status}"
                );
                break;
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("crash driver timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    // The driver is single-threaded at fork. The child deliberately holds its
    // inherited fd until killed; parent guard drop must explicitly unlock it.
    let (f, id) = Fixture::replaced();
    let view = recovery_inspect(&f.b, &id).unwrap();
    let checked = Command::new(&binary)
        .arg(f._dir.path().join("b-state"))
        .arg(&f.bp)
        .arg(&id)
        .arg(&view.token)
        .arg("fork-lock")
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );

    // Real SIGKILL immediately after successful journal/publication fsyncs.
    for phase in 1..=10 {
        let (f, id) = Fixture::replaced();
        let view = recovery_inspect(&f.b, &id).unwrap();
        run(&f, &id, &view.token, "restore", phase);
        let mut bytes = vec![fs::read(f.bp.join("file")).unwrap()];
        for entry in recovery_list(&f.b).unwrap().entries {
            let r = recovery_inspect(&f.b, &entry.id).unwrap();
            if r.retained.is_some() {
                bytes.push(
                    recovery_read(&f.b, &entry.id, RecoveryRead::Retained, &r.token).unwrap(),
                );
            }
        }
        assert!(
            bytes.iter().any(|b| b == b"original\0\xff"),
            "phase {phase}"
        );
        assert!(bytes.iter().any(|b| b == b"replacement"), "phase {phase}");
        // A fresh explicit keep-live decision can finish every journal left by
        // the interrupted restore, without modifying any saved content.
        for entry in recovery_list(&f.b).unwrap().entries {
            let r = recovery_inspect(&f.b, &entry.id).unwrap();
            if r.recorded {
                recovery_resolve(&f.b, &entry.id, &r.token, RecoveryChoice::KeepLive).unwrap();
            } else {
                recovery_discard(&f.b, &entry.id, &r.token).unwrap();
            }
        }
        assert!(!recovery_list(&f.b).unwrap().blocked);
    }
    for phase in [2, 4, 6, 7, 8, 9, 10] {
        let (f, id) = Fixture::replaced();
        fs::remove_file(f.bp.join("file")).unwrap();
        let before = recovery_inspect(&f.b, &id).unwrap();
        run(&f, &id, &before.token, "restore", phase);
        let restored = recovery_inspect(&f.b, &id).unwrap();
        assert_eq!(
            recovery_read(&f.b, &id, RecoveryRead::Retained, &restored.token).unwrap(),
            b"original\0\xff"
        );
        let list = recovery_list(&f.b).unwrap();
        if phase < 7 {
            assert!(
                list.entries.iter().any(|entry| entry.id != id),
                "creation journal must survive interruption at phase {phase}"
            );
        }
        for entry in list.entries {
            let view = recovery_inspect(&f.b, &entry.id).unwrap();
            if view.recorded {
                recovery_resolve(&f.b, &entry.id, &view.token, RecoveryChoice::KeepLive).unwrap();
            } else {
                recovery_discard(&f.b, &entry.id, &view.token).unwrap();
            }
        }
        assert!(!recovery_list(&f.b).unwrap().blocked);
    }
    for phase in 1..=4 {
        let (f, id) = Fixture::replaced();
        let view = recovery_inspect(&f.b, &id).unwrap();
        run(&f, &id, &view.token, "discard", phase);
        if receipt_path(&f.bp, &id).exists() {
            recovery_discard(&f.b, &id, &view.token).unwrap();
        }
        assert!(!receipt_path(&f.bp, &id).exists());
        assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"replacement");
    }
    // A malformed receipt's raw bytes have their own discard journal. Even
    // interrupted metadata disposal must preserve every saved content inode.
    for phase in 1..=4 {
        let (f, id) = Fixture::replaced();
        let original = recovery_inspect(&f.b, &id).unwrap();
        fs::write(receipt_path(&f.bp, &id), b"malformed metadata").unwrap();
        let view = recovery_inspect(&f.b, &id).unwrap();
        assert!(!view.recorded);
        run(&f, &id, &view.token, "discard", phase);
        for entry in recovery_list(&f.b).unwrap().entries {
            if entry.state == "discarding" {
                recovery_discard(&f.b, &entry.id, &view.token).unwrap();
            }
        }
        if receipt_path(&f.bp, &id).exists() {
            let fresh = recovery_inspect(&f.b, &id).unwrap();
            recovery_discard(&f.b, &id, &fresh.token).unwrap();
        }
        assert_eq!(
            fs::read(receipt_path(&f.bp, &original.receipt.version)).unwrap(),
            b"original\0\xff"
        );
        assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"replacement");
    }
}

#[test]
fn recovery_streaming_is_bounded_and_rejects_a_changed_version_between_chunks() {
    let f = Fixture::new();
    let root = f.bp.join(".clix-recovery");
    fs::create_dir(&root).unwrap();
    let bytes = (0..150000).map(|n| (n % 256) as u8).collect::<Vec<_>>();
    fs::write(root.join("saved"), &bytes).unwrap();
    let view = recovery_inspect(&f.b, "saved").unwrap();
    let mut export =
        clix::recovery_export(&f.b, "saved", RecoveryRead::Retained, &view.token).unwrap();
    assert!(
        export.finish().is_err(),
        "a partial stream is never success"
    );
    let mut read = Vec::new();
    loop {
        let chunk = export.read_chunk(65536).unwrap();
        assert_eq!(chunk.offset, read.len() as u64);
        assert_eq!(chunk.total_bytes, bytes.len() as u64);
        assert!(chunk.bytes.len() <= 65536);
        let eof = chunk.eof;
        read.extend(chunk.bytes);
        if eof {
            break;
        }
    }
    export.finish().unwrap();
    assert_eq!(read, bytes);
    drop(export);
    let mut export =
        clix::recovery_export(&f.b, "saved", RecoveryRead::Retained, &view.token).unwrap();
    assert!(export.read_chunk(65537).is_err());
    assert!(
        export.read_chunk(10).is_err(),
        "a failed stream cannot resume"
    );
    drop(export);
    let mut export =
        clix::recovery_export(&f.b, "saved", RecoveryRead::Retained, &view.token).unwrap();
    export.read_chunk(65536).unwrap();
    fs::write(root.join("saved"), b"changed").unwrap();
    assert!(export.read_chunk(65536).is_err());
    assert!(export.finish().is_err());
    drop(export);
    assert!(clix::recovery_export(&f.b, "saved", RecoveryRead::Retained, &view.token).is_err());
}

fn process_read_bytes() -> u64 {
    fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn large_export_and_repeated_live_listing_use_linear_file_reads() {
    let f = Fixture::new();
    let root = f.bp.join(".clix-recovery");
    fs::create_dir(&root).unwrap();
    let size = 16 * 1024 * 1024;
    let saved = vec![0x61u8; size];
    let live = vec![0x62u8; size];
    fs::write(root.join("saved"), &saved).unwrap();
    fs::write(f.bp.join("file"), &live).unwrap();
    let receipt: RecoveryReceipt = serde_json::from_value(json!({
        "path":"file", "version":"saved", "state":"retained",
        "expected":{"hash":format!("{:x}", Sha256::digest(&saved)),"mode":0o644},
        "replacement":{"hash":format!("{:x}", Sha256::digest(&live)),"mode":0o644}
    }))
    .unwrap();
    fs::set_permissions(root.join("saved"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(f.bp.join("file"), fs::Permissions::from_mode(0o644)).unwrap();
    write_receipt(&f.bp, "first.json", &receipt);
    let view = recovery_inspect(&f.b, "first.json").unwrap();
    let before = process_read_bytes();
    let mut export =
        clix::recovery_export(&f.b, "first.json", RecoveryRead::Retained, &view.token).unwrap();
    let mut hash = Sha256::new();
    let mut total = 0u64;
    loop {
        let chunk = export.read_chunk(65536).unwrap();
        total += chunk.bytes.len() as u64;
        hash.update(&chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    export.finish().unwrap();
    let read_bytes = process_read_bytes() - before;
    println!("16 MiB export logical read bytes: {read_bytes}");
    assert_eq!(total, size as u64);
    assert_eq!(
        format!("{:x}", hash.finalize()),
        receipt.expected.as_ref().unwrap().hash
    );
    // Initial live+saved inspection and one selected stream need ~48 MiB.
    // Permit other small tests in this process; the former per-chunk rehash
    // reads >8 GiB. This is a read-volume check, not a timing benchmark.
    assert!(
        read_bytes < 128 * 1024 * 1024,
        "export reread {read_bytes} bytes"
    );
    drop(export);
    for i in 0..15 {
        let version = format!("small-{i}");
        fs::write(root.join(&version), [i as u8]).unwrap();
        fs::set_permissions(root.join(&version), fs::Permissions::from_mode(0o644)).unwrap();
        let mut r = receipt.clone();
        r.version = version;
        r.expected = serde_json::from_value(
            json!({"hash":format!("{:x}",Sha256::digest([i as u8])),"mode":0o644}),
        )
        .unwrap();
        write_receipt(&f.bp, &format!("small-{i}.json"), &r);
    }
    let before = process_read_bytes();
    let list = recovery_list(&f.b).unwrap();
    let read_bytes = process_read_bytes() - before;
    println!("16 receipts sharing 16 MiB live file logical read bytes: {read_bytes}");
    assert_eq!(list.entries.len(), 16);
    assert!(!list.blocked);
    assert!(
        read_bytes < 128 * 1024 * 1024,
        "listing reread {read_bytes} bytes"
    );
}

#[test]
fn export_detects_namespace_changes_and_edits_before_terminal_success() {
    for mutation in 0..4 {
        let (f, id) = Fixture::replaced();
        let view = recovery_inspect(&f.b, &id).unwrap();
        let mut export =
            clix::recovery_export(&f.b, &id, RecoveryRead::Retained, &view.token).unwrap();
        export.read_chunk(4).unwrap();
        match mutation {
            0 => {
                // A same-length edit through an already open inode is detected.
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .open(receipt_path(&f.bp, &view.receipt.version))
                    .unwrap();
                file.write_all(b"CHANGED!").unwrap();
            }
            1 => {
                let path = receipt_path(&f.bp, &view.receipt.version);
                fs::rename(&path, receipt_path(&f.bp, "detached")).unwrap();
                fs::write(path, b"original\0\xff").unwrap();
            }
            2 => {
                fs::rename(&f.bp, f._dir.path().join("old-pin")).unwrap();
                fs::create_dir(&f.bp).unwrap();
            }
            3 => {
                // Even after the last bytes, a changed inspected live version
                // must prevent the terminal success frame.
                assert!(export.read_chunk(65536).unwrap().eof);
                fs::write(f.bp.join("file"), b"owner edited live").unwrap();
                assert!(export.finish().is_err());
                continue;
            }
            _ => unreachable!(),
        }
        assert!(export.read_chunk(65536).is_err(), "mutation {mutation}");
        assert!(export.finish().is_err());
    }
}

#[test]
fn legacy_over_limit_recovery_can_be_paged_inspected_and_explicitly_discarded() {
    let f = Fixture::new();
    let root = f.bp.join(".clix-recovery");
    fs::create_dir(&root).unwrap();
    let count = clix::MAX_RECOVERY_ENTRIES as usize + 20;
    for i in 0..count {
        fs::write(root.join(format!("saved-{i:05}")), b"old").unwrap();
    }
    let mut cursor = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = clix::recovery_list_page(&f.b, cursor.as_deref(), 128).unwrap();
        assert!(page.blocked);
        assert_eq!(page.usage.entries, count as u64);
        assert!(page.entries.len() <= 128);
        for entry in page.entries {
            assert!(ids.insert(entry.id));
        }
        if page.next_cursor.is_none() {
            break;
        }
        assert!(page.next_cursor > cursor);
        cursor = page.next_cursor;
    }
    assert_eq!(ids.len(), count);
    let id = ids.last().unwrap();
    let view = recovery_inspect(&f.b, id).unwrap();
    assert_eq!(
        recovery_read(&f.b, id, RecoveryRead::Retained, &view.token).unwrap(),
        b"old"
    );
    recovery_discard(&f.b, id, &view.token).unwrap();
    assert!(!root.join(id).exists());
    assert_eq!(
        clix::recovery_list_page(&f.b, None, 1)
            .unwrap()
            .usage
            .entries,
        count as u64 - 1
    );
    assert!(clix::recovery_list_page(&f.b, Some("../escape"), 1).is_err());
    assert!(clix::recovery_list_page(&f.b, None, 129).is_err());
}

#[test]
fn dangling_recovery_symlink_and_empty_directory_overflow_fail_closed() {
    let mut f = Fixture::new();
    std::os::unix::fs::symlink(f._dir.path().join("missing"), f.ap.join(".clix-recovery")).unwrap();
    fs::write(f.ap.join("file"), b"must not publish").unwrap();
    assert!(clix::pin_sync(&mut f.a, &f.ap, &mut f.b, &f.bp).is_err());
    assert!(!f.bp.join("file").exists());
    fs::remove_file(f.ap.join(".clix-recovery")).unwrap();
    for i in 0..4097 {
        fs::create_dir(f.ap.join(format!("empty-{i}"))).unwrap();
    }
    let error = clix::pin_sync(&mut f.a, &f.ap, &mut f.b, &f.bp).unwrap_err();
    assert!(error.to_string().contains("4096 entries"));
    assert!(!f.bp.join("file").exists());
}

fn poison_store(store: &mut Store, state: &Path) {
    let file = state.join("state.json");
    let saved = state.join("state.saved");
    fs::rename(&file, &saved).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(store.update(|_| Ok(())).is_err());
    fs::remove_dir(&file).unwrap();
    fs::rename(saved, file).unwrap();
    assert!(store.ensure_writable().is_err());
}

#[tokio::test]
async fn poisoned_store_refuses_inbound_and_outbound_pin_publication() {
    let n = network().await;
    poison_store(&mut n.b.lock().unwrap(), &n._dir.path().join("b"));
    let (peer, sk) = {
        let a = n.a.lock().unwrap();
        (a.peers[0].clone(), a.owner_sk.clone())
    };
    let error = clix::mesh_call(
        peer.addr.as_deref().unwrap(),
        &sk,
        &peer.owner_pk,
        json!({
            "op":"pin_put", "path":"file", "expected":null,
            "new":{"hash":format!("{:x}",Sha256::digest(b"x")),"mode":0o644}, "content":"78"
        }),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("storage is unavailable"));
    assert!(!n.bp.join("file").exists());
    // Reopening repaired storage clears the poison, as an actual restart does.
    let mut b = Store::open(&n._dir.path().join("b")).unwrap();
    b.pin_dir = Some(n.bp.clone());
    *n.b.lock().unwrap() = b;
    poison_store(&mut n.a.lock().unwrap(), &n._dir.path().join("a"));
    fs::write(n.ap.join("file"), b"must not dispatch").unwrap();
    assert!(clix::owner_pin_sync(&n.a, "beta").await.is_err());
    assert!(!n.bp.join("file").exists());
}

#[test]
fn recovery_context_outlives_store_and_malformed_metadata_is_only_a_raw_artifact() {
    let (f, id) = Fixture::replaced();
    let original = recovery_inspect(&f.b, &id).unwrap();
    let malformed = b"{broken receipt metadata\0\xff";
    fs::write(receipt_path(&f.bp, &id), malformed).unwrap();
    let context = clix::RecoveryContext::from_store(&f.b);
    let view = context.inspect(&id).unwrap();
    assert!(!view.recorded);
    assert!(view.receipt.path.is_empty());
    assert!(view.live.is_none());
    assert!(context.list_page(None, 128).unwrap().blocked);
    let mut export = context
        .export(&id, RecoveryRead::Retained, &view.token)
        .unwrap();
    let chunk = export.read_chunk(65536).unwrap();
    assert!(chunk.eof);
    assert_eq!(chunk.bytes, malformed);
    export.finish().unwrap();
    drop(export);
    assert!(recovery_resolve(&f.b, &id, &view.token, RecoveryChoice::RestoreRetained).is_err());
    recovery_discard(&f.b, &id, &view.token).unwrap();
    // Disposing of unusable metadata never disposes of its old content file.
    assert_eq!(
        fs::read(receipt_path(&f.bp, &original.receipt.version)).unwrap(),
        b"original\0\xff"
    );
    assert_eq!(fs::read(f.bp.join("file")).unwrap(), b"replacement");
    let saved = original.receipt.version;
    drop(f.b);
    let view = context.inspect(&saved).unwrap();
    let mut export = context
        .export(&saved, RecoveryRead::Retained, &view.token)
        .unwrap();
    assert_eq!(export.read_chunk(65536).unwrap().bytes, b"original\0\xff");
    export.finish().unwrap();
}
