//! Content-based replication. All file operations stay beneath an opened root.
//! Replaced/deleted versions are retained locally in .clix-recovery so a
//! concurrent writer's inode is never silently discarded by publication.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::Peer;
use rustix::fs::{Mode, OFlags, RenameFlags, ResolveFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const RECOVERY: &str = ".clix-recovery";
const MAX_FILE: u64 = 16 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    hash: String,
    mode: u32,
}
type Manifest = BTreeMap<String, Fingerprint>;
#[derive(Deserialize)]
struct Listing {
    files: Manifest,
    baseline: Manifest,
}
#[derive(Clone)]
struct Change {
    path: String,
    expected: Option<Fingerprint>,
    new: Option<Fingerprint>,
    to_remote: bool,
}

pub fn pin_dir() -> PathBuf {
    std::env::var_os("CLIX_PIN")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("src"))
}
pub fn pin_root(s: &Store) -> PathBuf {
    s.pin_dir.clone().unwrap_or_else(pin_dir)
}
fn key(pk: &[u8]) -> String {
    hex(pk)
}
fn lock(s: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}
fn conflict(path: &str, a: &str, b: &str) -> ClixError {
    ClixError::PinConflict {
        path: format!("src/{path}"),
        a: a.into(),
        b: b.into(),
    }
}
fn changed(path: &str) -> ClixError {
    ClixError::PinConflict {
        path: format!("src/{path}"),
        a: "scanned version".into(),
        b: "current version".into(),
    }
}
fn os(e: rustix::io::Errno) -> ClixError {
    std::io::Error::from(e).into()
}
fn valid_path(rel: &str) -> Result<()> {
    if rel.is_empty()
        || Path::new(rel)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || rel.split('/').any(|c| c == RECOVERY)
    {
        return Err(ClixError::Protocol("invalid pin path".into()));
    }
    Ok(())
}

struct Tree {
    root: File,
    path: PathBuf,
}
impl Tree {
    fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)?;
        let root: File = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(os)?
        .into();
        Ok(Self {
            root,
            path: fs::canonicalize(path)?,
        })
    }
    fn open_rel(&self, rel: &str, flags: OFlags) -> Result<File> {
        let fd = rustix::fs::openat2(
            &self.root,
            rel,
            flags | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )
        .map_err(os)?;
        Ok(fd.into())
    }
    fn read(&self, path: &str) -> Result<Option<(Fingerprint, Vec<u8>)>> {
        valid_path(path)?;
        let fd = match rustix::fs::openat2(
            &self.root,
            path,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(os(e)),
        };
        let mut file: File = fd.into();
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(ClixError::Protocol(format!(
                "pin supports regular files only: {path}"
            )));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(ClixError::Protocol(format!(
                "pin file exceeds 16 MiB: {path}"
            )));
        }
        let now = file.metadata()?;
        if meta.len() != now.len() || meta.modified()? != now.modified()? {
            return Err(changed(path));
        }
        Ok(Some((
            fingerprint(&bytes, now.permissions().mode() & 0o777),
            bytes,
        )))
    }
    fn scan(&self) -> Result<Manifest> {
        self.check_recovery()?;
        let mut result = Manifest::new();
        self.scan_dir(&self.root, "", &mut result)?;
        Ok(result)
    }
    fn check_recovery(&self) -> Result<()> {
        let recovery = match rustix::fs::openat2(
            &self.root,
            RECOVERY,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(fd) => File::from(fd),
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(e) => return Err(os(e)),
        };
        for ent in rustix::fs::Dir::read_from(&recovery).map_err(os)? {
            let ent = ent.map_err(os)?;
            let name = ent
                .file_name()
                .to_str()
                .map_err(|_| ClixError::Protocol("invalid recovery receipt name".into()))?;
            if !name.ends_with(".json") {
                continue;
            }
            let file: File = rustix::fs::openat(
                &recovery,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(os)?
            .into();
            let receipt: Value = serde_json::from_reader(file.take(65536))?;
            if receipt["state"] != "retained" {
                return Err(changed(&format!(
                    "{}; owner must review {RECOVERY}/{name} before syncing",
                    receipt["path"].as_str().unwrap_or("unknown path")
                )));
            }
            let version = receipt["version"].as_str().ok_or_else(|| changed(name))?;
            if version.contains('/') || matches!(version, "." | "..") {
                return Err(changed(name));
            }
            let mut saved: File = rustix::fs::openat(
                &recovery,
                version,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(os)?
            .into();
            if !saved.metadata()?.is_file() {
                return Err(changed(name));
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut saved)
                .take(MAX_FILE + 1)
                .read_to_end(&mut bytes)?;
            let expected: Fingerprint = serde_json::from_value(receipt["expected"].clone())?;
            if fingerprint(&bytes, saved.metadata()?.permissions().mode() & 0o777) != expected {
                return Err(changed(&format!(
                    "{}; retained version changed, review {RECOVERY}/{name}",
                    receipt["path"].as_str().unwrap_or("unknown path")
                )));
            }
        }
        Ok(())
    }

    fn scan_dir(&self, dir: &File, prefix: &str, out: &mut Manifest) -> Result<()> {
        for ent in rustix::fs::Dir::read_from(dir).map_err(os)? {
            let ent = ent.map_err(os)?;
            let name = ent
                .file_name()
                .to_str()
                .map_err(|_| ClixError::Protocol("pin requires UTF-8 filenames".into()))?;
            if matches!(name, "." | "..") || (prefix.is_empty() && name == RECOVERY) {
                continue;
            }
            let path = if prefix.is_empty() {
                name.into()
            } else {
                format!("{prefix}/{name}")
            };
            match ent.file_type() {
                rustix::fs::FileType::Directory => self.scan_dir(
                    &self.open_rel(&path, OFlags::RDONLY | OFlags::DIRECTORY)?,
                    &path,
                    out,
                )?,
                rustix::fs::FileType::RegularFile => {
                    let (fp, _) = self.read(&path)?.ok_or_else(|| changed(&path))?;
                    out.insert(path, fp);
                }
                _ => {
                    return Err(ClixError::Protocol(format!(
                        "unsupported pin entry (symlink or special file): {path}"
                    )))
                }
            }
        }
        Ok(())
    }
    fn parent(&self, path: &str) -> Result<(File, String)> {
        valid_path(path)?;
        let mut parts: Vec<&str> = path.split('/').collect();
        let name = parts.pop().unwrap().to_string();
        let mut parent = self.root.try_clone()?;
        for part in parts {
            match rustix::fs::mkdirat(&parent, part, Mode::from_raw_mode(0o755)) {
                Ok(()) => parent.sync_all()?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(os(e)),
            }
            parent = rustix::fs::openat2(
                &parent,
                part,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
            )
            .map_err(os)?
            .into();
        }
        Ok((parent, name))
    }
    fn apply(&self, change: &Change, bytes: Option<&[u8]>, store: &Store) -> Result<()> {
        self.check_recovery()?;
        if let Some(new) = &change.new {
            let data = bytes.ok_or_else(|| ClixError::Protocol("pin content missing".into()))?;
            if new.mode & !0o777 != 0 || fingerprint(data, new.mode) != *new {
                return Err(ClixError::Protocol(
                    "invalid pin content hash or mode".into(),
                ));
            }
        }
        let current = self.read(&change.path)?.map(|v| v.0);
        if current == change.new {
            return Ok(());
        } // Retry after publication before acknowledgement.
        if current != change.expected {
            return Err(changed(&change.path));
        }
        let destination = self.path.join(&change.path);
        for grant in &store.grants {
            let binary =
                fs::canonicalize(&grant.binary).unwrap_or(std::path::absolute(&grant.binary)?);
            if binary == destination {
                return Err(ClixError::Protocol(format!("pin would replace granted binary {}. Remove its grant on this machine before syncing, then review and add it again.", grant.tool)));
            }
        }
        let (parent, name) = self.parent(&change.path)?;
        match rustix::fs::mkdirat(&self.root, RECOVERY, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(os(e)),
        }
        let recovery = self.open_rel(RECOVERY, OFlags::RDONLY | OFlags::DIRECTORY)?;
        self.root.sync_all()?;
        let version = crate::job::new_id()?;
        let receipt_name = format!("{version}.json");
        if change.expected.is_some() {
            let receipt = json!({"path":change.path,"expected":change.expected,"replacement":change.new,"version":version,"state":"pending"});
            write_receipt(&recovery, &receipt_name, &receipt)?;
        }
        if let Some(new) = &change.new {
            let mut file: File = rustix::fs::openat(
                &recovery,
                &version,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(os)?
            .into();
            file.write_all(bytes.unwrap())?;
            file.set_permissions(fs::Permissions::from_mode(new.mode))?;
            file.sync_all()?;
            let flag = if current.is_some() {
                RenameFlags::EXCHANGE
            } else {
                RenameFlags::NOREPLACE
            };
            rustix::fs::renameat_with(&recovery, &version, &parent, &name, flag)
                .map_err(|_| changed(&change.path))?;
        } else {
            rustix::fs::renameat_with(&parent, &name, &recovery, &version, RenameFlags::NOREPLACE)
                .map_err(|_| changed(&change.path))?;
        }
        parent.sync_all()?;
        recovery.sync_all()?;
        if let Some(expected) = &change.expected {
            // The displaced inode remains recoverable, including writes through
            // handles opened before the rename. Never delete it on success.
            let mut previous: File = rustix::fs::openat(
                &recovery,
                &version,
                OFlags::RDONLY | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(os)?
            .into();
            let mut content = Vec::new();
            Read::by_ref(&mut previous)
                .take(MAX_FILE + 1)
                .read_to_end(&mut content)?;
            let actual = fingerprint(&content, previous.metadata()?.permissions().mode() & 0o777);
            if actual != *expected {
                return Err(changed(&format!("{} (review {RECOVERY}/{receipt_name}; previous version retained at {RECOVERY}/{version})", change.path)));
            }
            let receipt = json!({"path":change.path,"expected":expected,"replacement":change.new,"version":version,"state":"retained"});
            write_receipt(&recovery, &receipt_name, &receipt)?;
        }
        Ok(())
    }
}
fn write_receipt(dir: &File, name: &str, value: &Value) -> Result<()> {
    let tmp = format!("{}.tmp", crate::job::new_id()?);
    let mut file: File = rustix::fs::openat(
        dir,
        &tmp,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )
    .map_err(os)?
    .into();
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    rustix::fs::renameat(dir, &tmp, dir, name).map_err(os)?;
    dir.sync_all()?;
    Ok(())
}

fn fingerprint(bytes: &[u8], mode: u32) -> Fingerprint {
    Fingerprint {
        hash: hex(&Sha256::digest(bytes)),
        mode,
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || s.len() as u64 > MAX_FILE * 2 {
        return Err(ClixError::Protocol("invalid pin content".into()));
    }
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let h = (p[0] as char).to_digit(16);
            let l = (p[1] as char).to_digit(16);
            match (h, l) {
                (Some(h), Some(l)) => Ok((h * 16 + l) as u8),
                _ => Err(ClixError::Protocol("invalid pin content".into())),
            }
        })
        .collect()
}

fn plan(
    local: &Manifest,
    remote: &Manifest,
    baseline: &Manifest,
    a: &str,
    b: &str,
) -> Result<(Vec<Change>, Manifest)> {
    let paths: BTreeSet<_> = local
        .keys()
        .chain(remote.keys())
        .chain(baseline.keys())
        .cloned()
        .collect();
    let mut changes = Vec::new();
    let mut merged = Manifest::new();
    for path in paths {
        valid_path(&path)?;
        let l = local.get(&path);
        let r = remote.get(&path);
        let base = baseline.get(&path);
        let selected = if l == r {
            l
        } else if l == base {
            changes.push(Change {
                path: path.clone(),
                expected: l.cloned(),
                new: r.cloned(),
                to_remote: false,
            });
            r
        } else if r == base {
            changes.push(Change {
                path: path.clone(),
                expected: r.cloned(),
                new: l.cloned(),
                to_remote: true,
            });
            l
        } else {
            return Err(conflict(&path, a, b));
        };
        if let Some(fp) = selected {
            merged.insert(path, fp.clone());
        }
    }
    Ok((changes, merged))
}
fn baseline(s: &Store, peer: &[u8]) -> Manifest {
    s.pin_index
        .peers
        .get(&key(peer))
        .cloned()
        .unwrap_or_default()
}
fn common_baseline(
    a: Manifest,
    b: Manifest,
    local: &Manifest,
    remote: &Manifest,
) -> Result<Manifest> {
    if a == b {
        Ok(a)
    } else if local == remote {
        Ok(local.clone())
    } else {
        Err(conflict(
            "(sync baseline)",
            "local baseline",
            "peer baseline",
        ))
    }
}
fn record(s: &mut Store, pk: &[u8], manifest: Manifest) -> Result<()> {
    s.update(|s| {
        s.pin_index.hashes = manifest
            .iter()
            .map(|(p, f)| (p.clone(), f.hash.clone()))
            .collect();
        s.pin_index.peers.insert(key(pk), manifest);
        s.pin_index.last_sync = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        );
        s.pin_index.last_error = None;
        Ok(())
    })
}

pub fn sync(a: &mut Store, ap: &Path, b: &mut Store, bp: &Path) -> Result<()> {
    let at = Tree::open(ap)?;
    let bt = Tree::open(bp)?;
    let af = at.scan()?;
    let bf = bt.scan()?;
    let apk = crate::pair::owner_pk(&a.owner_sk)?;
    let bpk = crate::pair::owner_pk(&b.owner_sk)?;
    let base = common_baseline(baseline(a, &bpk), baseline(b, &apk), &af, &bf)?;
    let (changes, merged) = plan(&af, &bf, &base, &a.body_name, &b.body_name)?;
    for c in &changes {
        let (source, dest) = if c.to_remote { (&at, &bt) } else { (&bt, &at) };
        let data = source.read(&c.path)?;
        if data.as_ref().map(|v| &v.0) != c.new.as_ref() {
            return Err(changed(&c.path));
        }
        dest.apply(
            c,
            data.as_ref().map(|v| v.1.as_slice()),
            if c.to_remote { b } else { a },
        )?;
    }
    if at.scan()? != merged || bt.scan()? != merged {
        return Err(changed("(tree changed during sync)"));
    }
    record(a, &bpk, merged.clone())?;
    record(b, &apk, merged)
}

pub async fn sync_with_peer(
    store: &Arc<Mutex<Store>>,
    sk: &[u8],
    addr: &str,
    name: &str,
) -> Result<()> {
    let (peer, root, local_name) = {
        let s = lock(store);
        (
            s.peers
                .iter()
                .find(|p| p.name.0 == name)
                .cloned()
                .ok_or_else(|| ClixError::Usage(format!("{name} is not paired")))?,
            pin_root(&s),
            s.body_name.clone(),
        )
    };
    let tree = Tree::open(&root)?;
    let local = tree.scan()?;
    let remote: Listing = serde_json::from_value(
        crate::mesh::call(addr, sk, &peer.owner_pk, json!({"op":"pin_list"})).await?,
    )?;
    let base = common_baseline(
        baseline(&lock(store), &peer.owner_pk),
        remote.baseline,
        &local,
        &remote.files,
    )?;
    let (changes, merged) = plan(&local, &remote.files, &base, &local_name, name)?;
    for c in &changes {
        if c.to_remote {
            let data = tree.read(&c.path)?;
            if data.as_ref().map(|v| &v.0) != c.new.as_ref() {
                return Err(changed(&c.path));
            }
            crate::mesh::call(addr,sk,&peer.owner_pk,json!({"op":"pin_put","path":c.path,"expected":c.expected,"new":c.new,"content":data.as_ref().map(|v|hex(&v.1))})).await?;
        } else {
            let bytes = if c.new.is_some() {
                let v = crate::mesh::call(
                    addr,
                    sk,
                    &peer.owner_pk,
                    json!({"op":"pin_get","path":c.path,"expected":c.new}),
                )
                .await?;
                Some(unhex(
                    v.get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(|| ClixError::Protocol("missing pin content".into()))?,
                )?)
            } else {
                None
            };
            tree.apply(c, bytes.as_deref(), &lock(store))?;
        }
    }
    if tree.scan()? != merged {
        return Err(changed("(tree changed during sync)"));
    }
    crate::mesh::call(
        addr,
        sk,
        &peer.owner_pk,
        json!({"op":"pin_commit","manifest":merged}),
    )
    .await?;
    if tree.scan()? != merged {
        return Err(changed("(tree changed during commit)"));
    }
    record(&mut lock(store), &peer.owner_pk, merged)
}
pub async fn sync_after_pair(store: &Arc<Mutex<Store>>, sk: &[u8], peer: &Peer) -> Result<()> {
    sync_with_peer(
        store,
        sk,
        peer.addr.as_deref().ok_or(ClixError::Unreachable)?,
        &peer.name.0,
    )
    .await
}
pub(crate) fn rpc_list(s: &Store, peer: &Peer) -> Result<Value> {
    Ok(json!({"files":Tree::open(&pin_root(s))?.scan()?,"baseline":baseline(s,&peer.owner_pk)}))
}
pub(crate) fn rpc_get(s: &Store, req: &Value) -> Result<Value> {
    let path = req
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Protocol("missing pin path".into()))?;
    let expected: Fingerprint =
        serde_json::from_value(req.get("expected").cloned().unwrap_or(Value::Null))?;
    let (actual, bytes) = Tree::open(&pin_root(s))?
        .read(path)?
        .ok_or_else(|| changed(path))?;
    if actual != expected {
        return Err(changed(path));
    }
    Ok(json!({"content":hex(&bytes)}))
}
pub(crate) fn rpc_put(s: &Store, req: &Value) -> Result<Value> {
    let path = req
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Protocol("missing pin path".into()))?
        .to_string();
    if req.get("expected").is_none() || req.get("new").is_none() {
        return Err(ClixError::Protocol(
            "pin write requires version preconditions".into(),
        ));
    }
    let expected: Option<Fingerprint> = serde_json::from_value(req["expected"].clone())?;
    let new: Option<Fingerprint> = serde_json::from_value(req["new"].clone())?;
    let bytes = req
        .get("content")
        .and_then(Value::as_str)
        .map(unhex)
        .transpose()?;
    Tree::open(&pin_root(s))?.apply(
        &Change {
            path,
            expected,
            new,
            to_remote: false,
        },
        bytes.as_deref(),
        s,
    )?;
    Ok(json!({"ok":true}))
}
pub(crate) fn rpc_commit(s: &mut Store, peer: &Peer, req: &Value) -> Result<Value> {
    let manifest: Manifest =
        serde_json::from_value(req.get("manifest").cloned().unwrap_or(Value::Null))?;
    if Tree::open(&pin_root(s))?.scan()? != manifest {
        return Err(changed("(tree changed before commit)"));
    }
    record(s, &peer.owner_pk, manifest)?;
    Ok(json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confined_reads_reject_symlinks_and_writes_require_the_scanned_version() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("file"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let tree = Tree::open(root.path()).unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let state = Store::open(state_dir.path()).unwrap();
        assert!(tree.read("link/file").is_err());
        fs::write(root.path().join("ordinary"), "base").unwrap();
        let old = tree.read("ordinary").unwrap().unwrap().0;
        fs::write(root.path().join("ordinary"), "owner edit after scan").unwrap();
        let change = Change {
            path: "ordinary".into(),
            expected: Some(old),
            new: Some(fingerprint(b"remote edit", 0o644)),
            to_remote: false,
        };
        assert!(tree.apply(&change, Some(b"remote edit"), &state).is_err());
        assert_eq!(
            fs::read(root.path().join("ordinary")).unwrap(),
            b"owner edit after scan"
        );
        assert_eq!(fs::read(outside.path().join("file")).unwrap(), b"outside");
    }
}
