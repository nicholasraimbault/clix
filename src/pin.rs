//! Content-based replication. All file operations stay beneath an opened root.
//! Replaced/deleted versions are retained locally in .clix-recovery so a
//! concurrent writer's inode is never silently discarded by publication.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
const MAX_RECEIPT: u64 = 64 * 1024;
pub const MAX_RECOVERY_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_RECOVERY_ENTRIES: u64 = 4096;
const RECOVERY_RESERVE: u64 = 1024 * 1024;
const RECOVERY_LOCK: &str = ".lock";
const MAX_PIN_ENTRIES: usize = 4096;
pub const MAX_RECOVERY_PAGE: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub hash: String,
    pub mode: u32,
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

/// Pin is opt-in per machine. Every entry point that synchronizes, or that lets
/// a peer list, read or write this machine's pin tree, checks this first.
pub(crate) fn ensure_enabled(s: &Store) -> Result<()> {
    if !s.pin_enabled {
        return Err(ClixError::Usage(format!(
            "pin is off on {}; enable it there with clix pin on",
            s.body_name
        )));
    }
    Ok(())
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
        || excluded(rel)
    {
        return Err(ClixError::Protocol("invalid pin path".into()));
    }
    Ok(())
}

/// Paths pin never syncs or accepts, beyond `.clix-recovery`. Git hooks are
/// executable files that run on ordinary git operations, so a peer must not be
/// able to place one through pin, and local hooks are not replicated.
fn excluded(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    parts.windows(2).any(|w| w == [".git", "hooks"])
}

struct Tree {
    root: File,
    path: PathBuf,
}
struct RecoveryLock(File);
impl Drop for RecoveryLock {
    fn drop(&mut self) {
        // Explicit unlock also releases the shared open-file-description lock
        // if an unrelated concurrent fork briefly inherited the CLOEXEC fd.
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
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
        self.scan_dir(&self.root, "", &mut result, &mut 0)?;
        Ok(result)
    }
    fn check_recovery(&self) -> Result<()> {
        self.check_recovery_except(&[])
    }
    fn check_recovery_except(&self, ignored: &[&str]) -> Result<()> {
        let Some(recovery) = self.recovery_dir(false)? else {
            return Ok(());
        };
        let usage = recovery_usage(&recovery)?;
        if usage.bytes > MAX_RECOVERY_BYTES || usage.entries > MAX_RECOVERY_ENTRIES {
            return Err(ClixError::Usage("pin recovery storage is over its limit; use clix pin list and review retained versions".into()));
        }
        let mut referenced = BTreeSet::new();
        let names = recovery_names(&recovery)?;
        for name in &names {
            if !name.ends_with(".json") || ignored.contains(&name.as_str()) {
                continue;
            }
            let raw = read_internal(&recovery, name, MAX_RECEIPT)?;
            let value: Value = serde_json::from_slice(&raw).map_err(|e| {
                ClixError::Protocol(format!("invalid recovery receipt {name}: {e}"))
            })?;
            let path = value["path"].as_str().unwrap_or("unknown path");
            if !matches!(value["state"].as_str(), Some("retained" | "resolved")) {
                return Err(changed(&format!(
                    "{path}; owner must review {RECOVERY}/{name} before syncing"
                )));
            }
            let receipt = parse_receipt(&raw, name)?;
            referenced.insert(receipt.version.clone());
            let actual = internal_snapshot(&recovery, &receipt.version)?;
            if actual.as_ref().map(|v| &v.fingerprint) != receipt.expected.as_ref() {
                return Err(changed(&format!(
                    "{path}; retained version changed, review {RECOVERY}/{name}"
                )));
            }
        }
        for name in names {
            if !name.ends_with(".json") && !referenced.contains(&name) {
                // An ignored receipt can own a staged file during explicit recovery.
                let ignored_owner = ignored.iter().any(|id| {
                    read_internal(&recovery, id, MAX_RECEIPT)
                        .ok()
                        .and_then(|b| parse_receipt(&b, id).ok())
                        .is_some_and(|r| r.version == name)
                });
                if !ignored_owner {
                    return Err(changed(&format!(
                        "unreferenced recovery data {name}; inspect it before syncing"
                    )));
                }
            }
        }
        Ok(())
    }
    fn recovery_dir(&self, create: bool) -> Result<Option<File>> {
        if create {
            match rustix::fs::mkdirat(&self.root, RECOVERY, Mode::from_raw_mode(0o700)) {
                Ok(()) => self.root.sync_all()?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(os(e)),
            }
        }
        match rustix::fs::openat2(
            &self.root,
            RECOVERY,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(fd) => Ok(Some(File::from(fd))),
            Err(rustix::io::Errno::NOENT) if !create => Ok(None),
            Err(e) => Err(os(e)),
        }
    }
    fn recovery_lock(&self) -> Result<(File, RecoveryLock)> {
        let dir = self
            .recovery_dir(true)?
            .expect("created recovery directory");
        let file: File = rustix::fs::openat(
            &dir,
            RECOVERY_LOCK,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(os)?
        .into();
        if !file.metadata()?.is_file() {
            return Err(ClixError::Protocol("invalid pin recovery lock".into()));
        }
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
            |_| {
                ClixError::Usage(
                    "another pin operation is in progress; retry after it completes".into(),
                )
            },
        )?;
        Ok((dir, RecoveryLock(file)))
    }

    fn scan_dir(
        &self,
        dir: &File,
        prefix: &str,
        out: &mut Manifest,
        count: &mut usize,
    ) -> Result<()> {
        for ent in rustix::fs::Dir::read_from(dir).map_err(os)? {
            let ent = ent.map_err(os)?;
            let name = ent
                .file_name()
                .to_str()
                .map_err(|_| ClixError::Protocol("pin requires UTF-8 filenames".into()))?;
            if matches!(name, "." | "..") || (prefix.is_empty() && name == RECOVERY) {
                continue;
            }
            *count += 1;
            if *count > MAX_PIN_ENTRIES || (!prefix.is_empty() && prefix.split('/').count() >= 64) {
                return Err(ClixError::Usage(
                    "pin tree exceeds 4096 entries or 64 directories of depth".into(),
                ));
            }
            let path = if prefix.is_empty() {
                name.into()
            } else {
                format!("{prefix}/{name}")
            };
            // Never let excluded paths (e.g. .git/hooks) enter the manifest, so
            // they are neither read from nor written to during sync.
            if excluded(&path) {
                continue;
            }
            match ent.file_type() {
                rustix::fs::FileType::Directory => self.scan_dir(
                    &self.open_rel(&path, OFlags::RDONLY | OFlags::DIRECTORY)?,
                    &path,
                    out,
                    count,
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
        let (recovery, _guard) = self.recovery_lock()?;
        self.apply_locked(change, bytes, store, &recovery, &[])
            .map(|_| ())
    }
    fn apply_locked(
        &self,
        change: &Change,
        bytes: Option<&[u8]>,
        store: &Store,
        recovery: &File,
        ignored: &[&str],
    ) -> Result<Option<String>> {
        store.ensure_writable()?;
        self.check_recovery_except(ignored)?;
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
            return Ok(None);
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
        let old_bytes = self
            .read(&change.path)?
            .map(|(_, b)| b.len() as u64)
            .unwrap_or(0);
        admit_recovery(
            recovery,
            old_bytes.saturating_add(bytes.map_or(0, |b| b.len() as u64)),
            4,
        )?;
        let (parent, name) = self.parent(&change.path)?;
        let version = crate::job::new_id()?;
        let receipt_name = format!("{version}.json");
        let mut receipt = RecoveryReceipt {
            path: change.path.clone(),
            expected: change.expected.clone(),
            replacement: change.new.clone(),
            version: version.clone(),
            state: RecoveryState::Pending,
            source_receipt: ignored.first().map(|s| (*s).to_owned()),
            discard_token: None,
        };
        write_typed_receipt(recovery, &receipt_name, &receipt)?;
        if let Some(new) = &change.new {
            let mut file: File = rustix::fs::openat(
                recovery,
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
            rustix::fs::renameat_with(recovery, &version, &parent, &name, flag)
                .map_err(|_| changed(&change.path))?;
        } else {
            rustix::fs::renameat_with(&parent, &name, recovery, &version, RenameFlags::NOREPLACE)
                .map_err(|_| changed(&change.path))?;
        }
        parent.sync_all()?;
        recovery.sync_all()?;
        if let Some(expected) = &change.expected {
            // The displaced inode remains recoverable, including writes through
            // handles opened before the rename. Never delete it on success.
            let mut previous: File = rustix::fs::openat(
                recovery,
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
            previous.sync_all()?;
            receipt.state = RecoveryState::Retained;
            write_typed_receipt(recovery, &receipt_name, &receipt)?;
        }
        if change.expected.is_none() {
            // Creation has a journal before publication too; after successful
            // durability there is no displaced inode requiring retention.
            receipt.state = RecoveryState::Resolved;
            write_typed_receipt(recovery, &receipt_name, &receipt)?;
            // Only empty, completed creation metadata is removed automatically.
            // No displaced inode or recoverable content exists for this journal.
            rustix::fs::unlinkat(recovery, &receipt_name, rustix::fs::AtFlags::empty())
                .map_err(os)?;
            recovery.sync_all()?;
        }
        Ok(Some(receipt_name))
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
    a.ensure_writable()?;
    b.ensure_writable()?;
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

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ClixError::Io(format!("pin file operation failed: {e}")))?
}
fn current_sync_authority(store: &Store, peer: &Peer, root: &Path) -> Result<()> {
    store.ensure_writable()?;
    if pin_root(store) != root
        || !store
            .peers
            .iter()
            .any(|p| p.name == peer.name && p.owner_pk == peer.owner_pk)
    {
        return Err(changed(
            "pin root or peer identity changed during synchronization",
        ));
    }
    Ok(())
}
async fn open_scan(root: PathBuf) -> Result<(Arc<Tree>, Manifest)> {
    blocking(move || {
        let tree = Arc::new(Tree::open(&root)?);
        let files = tree.scan()?;
        Ok((tree, files))
    })
    .await
}
async fn scan(tree: &Arc<Tree>) -> Result<Manifest> {
    let tree = tree.clone();
    blocking(move || tree.scan()).await
}
pub async fn sync_with_peer(
    store: &Arc<Mutex<Store>>,
    sk: &[u8],
    addr: &str,
    name: &str,
) -> Result<()> {
    let (peer, root, local_name) = {
        let s = lock(store);
        s.ensure_writable()?;
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
    let (tree, local) = open_scan(root.clone()).await?;
    let remote: Listing = serde_json::from_value(
        crate::mesh::call(addr, sk, &peer.owner_pk, json!({"op":"pin_list"})).await?,
    )?;
    validate_listing(&remote)?;
    let base = common_baseline(
        baseline(&lock(store), &peer.owner_pk),
        remote.baseline,
        &local,
        &remote.files,
    )?;
    let (changes, merged) = plan(&local, &remote.files, &base, &local_name, name)?;
    for c in changes {
        if c.to_remote {
            let source = tree.clone();
            let path = c.path.clone();
            let data = blocking(move || source.read(&path)).await?;
            if data.as_ref().map(|v| &v.0) != c.new.as_ref() {
                return Err(changed(&c.path));
            }
            current_sync_authority(&lock(store), &peer, &root)?;
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
            let destination = tree.clone();
            let state = store.clone();
            let selected_peer = peer.clone();
            let selected_root = root.clone();
            blocking(move || {
                let s = lock(&state);
                current_sync_authority(&s, &selected_peer, &selected_root)?;
                destination.apply(&c, bytes.as_deref(), &s)
            })
            .await?;
        }
    }
    if scan(&tree).await? != merged {
        return Err(changed("(tree changed during sync)"));
    }
    current_sync_authority(&lock(store), &peer, &root)?;
    crate::mesh::call(
        addr,
        sk,
        &peer.owner_pk,
        json!({"op":"pin_commit","manifest":merged}),
    )
    .await?;
    if scan(&tree).await? != merged {
        return Err(changed("(tree changed during commit)"));
    }
    let state = store.clone();
    blocking(move || {
        let mut s = lock(&state);
        current_sync_authority(&s, &peer, &root)?;
        record(&mut s, &peer.owner_pk, merged)
    })
    .await
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
pub(crate) async fn rpc_list_async(store: &Arc<Mutex<Store>>, peer: &Peer) -> Result<Value> {
    let (root, baseline) = {
        let s = lock(store);
        ensure_enabled(&s)?;
        (pin_root(&s), baseline(&s, &peer.owner_pk))
    };
    blocking(move || Ok(json!({"files":Tree::open(&root)?.scan()?,"baseline":baseline}))).await
}
pub(crate) async fn rpc_get_async(store: &Arc<Mutex<Store>>, request: &Value) -> Result<Value> {
    let root = {
        let s = lock(store);
        ensure_enabled(&s)?;
        pin_root(&s)
    };
    let request = request.clone();
    blocking(move || {
        let path = request
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ClixError::Protocol("missing pin path".into()))?;
        let expected: Fingerprint =
            serde_json::from_value(request.get("expected").cloned().unwrap_or(Value::Null))?;
        let (actual, bytes) = Tree::open(&root)?
            .read(path)?
            .ok_or_else(|| changed(path))?;
        if actual != expected {
            return Err(changed(path));
        }
        Ok(json!({"content":hex(&bytes)}))
    })
    .await
}
pub(crate) async fn rpc_put_async(
    store: &Arc<Mutex<Store>>,
    peer: &Peer,
    request: &Value,
) -> Result<Value> {
    let store = store.clone();
    let request = request.clone();
    let peer = peer.clone();
    blocking(move || rpc_put(&lock(&store), &peer, &request)).await
}
pub(crate) async fn rpc_commit_async(
    store: &Arc<Mutex<Store>>,
    peer: &Peer,
    request: &Value,
) -> Result<Value> {
    let store = store.clone();
    let request = request.clone();
    let peer = peer.clone();
    blocking(move || rpc_commit(&mut lock(&store), &peer, &request)).await
}
pub(crate) fn rpc_put(s: &Store, peer: &Peer, req: &Value) -> Result<Value> {
    ensure_enabled(s)?;
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
    // Enforce the honest-conflict rule on the receiver, not only in the
    // initiator's planner: a write is accepted only when the receiver's own
    // recorded baseline for this peer matches what the caller expected. If the
    // receiver has a local edit since the last sync (baseline != current), a
    // peer cannot overwrite it by reading and echoing the current fingerprint.
    valid_path(&path)?;
    if baseline(s, &peer.owner_pk).get(&path) != expected.as_ref() {
        return Err(conflict(
            &path,
            &peer.name.0,
            &format!("{} (local change since last sync)", s.body_name),
        ));
    }
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
    ensure_enabled(s)?;
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

// Owner recovery operations. Mesh dispatch must never expose these methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    Pending,
    Retained,
    Resolved,
    Discarding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryReceipt {
    pub path: String,
    pub expected: Option<Fingerprint>,
    pub replacement: Option<Fingerprint>,
    pub version: String,
    pub state: RecoveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryVersion {
    pub fingerprint: Fingerprint,
    pub bytes: u64,
    pub device: u64,
    pub inode: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub changed_seconds: i64,
    pub changed_nanoseconds: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryInspection {
    pub id: String,
    pub recorded: bool,
    pub receipt: RecoveryReceipt,
    pub live: Option<RecoveryVersion>,
    pub retained: Option<RecoveryVersion>,
    pub token: String,
    /// Pending publication is never assumed to have completed from the receipt alone.
    pub explanation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryEntry {
    pub id: String,
    pub path: Option<String>,
    pub state: String,
    pub problem: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryUsage {
    pub bytes: u64,
    pub entries: u64,
    pub limit_bytes: u64,
    pub limit_entries: u64,
    pub admission_blocked: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryList {
    pub body: String,
    pub usage: RecoveryUsage,
    pub blocked: bool,
    pub entries: Vec<RecoveryEntry>,
    pub next_cursor: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryChoice {
    KeepLive,
    RestoreRetained,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRead {
    Receipt,
    Live,
    Retained,
}

fn valid_recovery_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name == RECOVERY_LOCK
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(ClixError::Protocol("invalid recovery ID".into()));
    }
    Ok(())
}
fn parse_receipt(bytes: &[u8], id: &str) -> Result<RecoveryReceipt> {
    let receipt: RecoveryReceipt = serde_json::from_slice(bytes)
        .map_err(|e| ClixError::Protocol(format!("invalid recovery receipt {id}: {e}")))?;
    if !receipt.path.is_empty() || receipt.state != RecoveryState::Discarding {
        valid_path(&receipt.path)?;
    }
    valid_recovery_name(&receipt.version)?;
    if (receipt.version.ends_with(".json")
        && !(receipt.state == RecoveryState::Discarding && receipt.path.is_empty()))
        || (receipt.version.ends_with(".tmp") && receipt.state != RecoveryState::Discarding)
    {
        return Err(ClixError::Protocol(
            "recovery version must name a data file".into(),
        ));
    }
    Ok(receipt)
}
fn write_typed_receipt(dir: &File, id: &str, receipt: &RecoveryReceipt) -> Result<()> {
    valid_recovery_name(id)?;
    write_receipt(dir, id, &serde_json::to_value(receipt)?)
}
fn open_internal(dir: &File, name: &str) -> Result<Option<File>> {
    valid_recovery_name(name)?;
    match rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => {
            let file = File::from(fd);
            if !file.metadata()?.is_file() {
                return Err(ClixError::Protocol(
                    "recovery entries must be regular files".into(),
                ));
            }
            Ok(Some(file))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(e) => Err(os(e)),
    }
}
fn read_internal(dir: &File, name: &str, max: u64) -> Result<Vec<u8>> {
    let file = open_internal(dir, name)?
        .ok_or_else(|| ClixError::Usage("recovery version is absent".into()))?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(ClixError::Usage(
            "recovery read exceeds the supported size; data was preserved".into(),
        ));
    }
    Ok(bytes)
}
fn same_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn matches_version(meta: &fs::Metadata, version: &RecoveryVersion) -> bool {
    meta.is_file()
        && meta.dev() == version.device
        && meta.ino() == version.inode
        && meta.len() == version.bytes
        && meta.mode() & 0o777 == version.fingerprint.mode
        && meta.mtime() == version.modified_seconds
        && meta.mtime_nsec() == version.modified_nanoseconds
        && meta.ctime() == version.changed_seconds
        && meta.ctime_nsec() == version.changed_nanoseconds
}
fn version_from_metadata(meta: &fs::Metadata, hash: String) -> RecoveryVersion {
    RecoveryVersion {
        fingerprint: Fingerprint {
            hash,
            mode: meta.mode() & 0o777,
        },
        bytes: meta.len(),
        device: meta.dev(),
        inode: meta.ino(),
        modified_seconds: meta.mtime(),
        modified_nanoseconds: meta.mtime_nsec(),
        changed_seconds: meta.ctime(),
        changed_nanoseconds: meta.ctime_nsec(),
    }
}
fn snapshot_file(mut file: File) -> Result<RecoveryVersion> {
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(ClixError::Protocol(
            "recovery supports regular files only".into(),
        ));
    }
    // Constant memory; stop at the size observed on opening, even if another
    // process continuously appends through a retained descriptor.
    let mut remaining = before.len();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let count = file.read(&mut buffer[..remaining.min(65536) as usize])?;
        if count == 0 {
            return Err(changed("recovery version changed while inspecting"));
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let after = file.metadata()?;
    if !same_metadata(&before, &after) {
        return Err(changed("recovery version changed while inspecting"));
    }
    Ok(version_from_metadata(&after, hex(&hash.finalize())))
}
fn internal_snapshot(dir: &File, name: &str) -> Result<Option<RecoveryVersion>> {
    open_internal(dir, name)?.map(snapshot_file).transpose()
}
fn open_live(tree: &Tree, path: &str) -> Result<Option<File>> {
    valid_path(path)?;
    match rustix::fs::openat2(
        &tree.root,
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(fd) => Ok(Some(File::from(fd))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(e) => Err(os(e)),
    }
}
type LiveCache = BTreeMap<String, Option<RecoveryVersion>>;
fn cached_live_snapshot(
    tree: &Tree,
    path: &str,
    cache: &mut LiveCache,
) -> Result<Option<RecoveryVersion>> {
    let file = open_live(tree, path)?;
    if let Some(cached) = cache.get(path) {
        let current = file.as_ref().map(File::metadata).transpose()?;
        if match (&current, cached) {
            (Some(meta), Some(version)) => matches_version(meta, version),
            (None, None) => true,
            _ => false,
        } {
            return Ok(cached.clone());
        }
        // Do not combine conflicting observations into a healthy listing.
        return Err(changed("live version changed during recovery listing"));
    }
    let version = file.map(snapshot_file).transpose()?;
    cache.insert(path.into(), version.clone());
    Ok(version)
}
fn recovery_names(dir: &File) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir).map_err(os)? {
        let entry = entry.map_err(os)?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| ClixError::Protocol("recovery filename must be UTF-8".into()))?;
        if matches!(name, "." | ".." | RECOVERY_LOCK) {
            continue;
        }
        names.push(name.to_owned());
        // Preserve everything but refuse an unbounded result supplied by an
        // external owner process. Admission already stops before this size.
        if names.len() > MAX_RECOVERY_ENTRIES as usize + 16 {
            return Err(ClixError::Usage(
                "recovery directory exceeds its entry limit; no data was deleted".into(),
            ));
        }
    }
    names.sort();
    Ok(names)
}
fn for_each_recovery_name(dir: &File, mut visit: impl FnMut(&str) -> Result<()>) -> Result<()> {
    for entry in rustix::fs::Dir::read_from(dir).map_err(os)? {
        let entry = entry.map_err(os)?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| ClixError::Protocol("recovery filename must be UTF-8".into()))?;
        if !matches!(name, "." | ".." | RECOVERY_LOCK) {
            valid_recovery_name(name)?;
            visit(name)?;
        }
    }
    Ok(())
}
fn usage_from_totals(bytes: u64, entries: u64) -> RecoveryUsage {
    RecoveryUsage {
        bytes,
        entries,
        limit_bytes: MAX_RECOVERY_BYTES,
        limit_entries: MAX_RECOVERY_ENTRIES,
        admission_blocked: bytes.saturating_add(RECOVERY_RESERVE) >= MAX_RECOVERY_BYTES
            || entries.saturating_add(8) >= MAX_RECOVERY_ENTRIES,
    }
}
fn recovery_usage(dir: &File) -> Result<RecoveryUsage> {
    let mut bytes = 0u64;
    let mut entries = 0u64;
    for_each_recovery_name(dir, |name| {
        let info =
            rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).map_err(os)?;
        bytes = bytes.saturating_add(info.st_size.max(0) as u64);
        entries = entries.saturating_add(1);
        Ok(())
    })?;
    Ok(usage_from_totals(bytes, entries))
}
fn admit_recovery(dir: &File, extra_bytes: u64, extra_entries: u64) -> Result<()> {
    let usage = recovery_usage(dir)?;
    if usage
        .bytes
        .saturating_add(extra_bytes)
        .saturating_add(RECOVERY_RESERVE)
        > MAX_RECOVERY_BYTES
        || usage
            .entries
            .saturating_add(extra_entries)
            .saturating_add(8)
            > MAX_RECOVERY_ENTRIES
    {
        return Err(ClixError::Usage(format!("pin recovery capacity reached ({} bytes, {} entries); review retained versions before syncing", usage.bytes, usage.entries)));
    }
    Ok(())
}
fn artifact_inspection(dir: &File, id: &str, explanation: &str) -> Result<RecoveryInspection> {
    for_each_recovery_name(dir, |name| {
        if name != id && name.ends_with(".json") {
            let parsed =
                read_internal(dir, name, MAX_RECEIPT).and_then(|b| parse_receipt(&b, name));
            match parsed {
                Ok(receipt) if receipt.version == id => {
                    return Err(ClixError::Usage(format!(
                        "this version belongs to receipt {name}; inspect that ID"
                    )))
                }
                // A malformed metadata file may be reviewed/discarded without
                // choosing any data version named by another malformed file.
                Err(error) if !id.ends_with(".json") => return Err(error),
                _ => {}
            }
        }
        Ok(())
    })?;
    let retained = internal_snapshot(dir, id)?
        .ok_or_else(|| ClixError::Usage("recovery artifact is absent".into()))?;
    let token = hex(&Sha256::digest(serde_json::to_vec(&(id, &retained))?));
    Ok(RecoveryInspection {
        id: id.into(),
        recorded: false,
        receipt: RecoveryReceipt {
            path: String::new(),
            expected: Some(retained.fingerprint.clone()),
            replacement: None,
            version: id.into(),
            state: RecoveryState::Resolved,
            source_receipt: None,
            discard_token: None,
        },
        live: None,
        retained: Some(retained),
        token,
        explanation: format!(
            "{explanation}; export retained bytes or explicitly discard only this artifact"
        ),
    })
}
fn inspection(tree: &Tree, dir: &File, id: &str) -> Result<RecoveryInspection> {
    inspection_cached(tree, dir, id, &mut LiveCache::new())
}
fn inspection_cached(
    tree: &Tree,
    dir: &File,
    id: &str,
    cache: &mut LiveCache,
) -> Result<RecoveryInspection> {
    valid_recovery_name(id)?;
    if !id.ends_with(".json") {
        return artifact_inspection(
            dir,
            id,
            "unreferenced saved or staged data; no original path is inferred",
        );
    }
    let receipt_file = open_internal(dir, id)?
        .ok_or_else(|| ClixError::Usage("recovery receipt is absent".into()))?;
    if receipt_file.metadata()?.len() > MAX_RECEIPT {
        return artifact_inspection(
            dir,
            id,
            "receipt exceeds 64 KiB; raw metadata only, no original path is inferred",
        );
    }
    let receipt = match parse_receipt(&read_internal(dir, id, MAX_RECEIPT)?, id) {
        Ok(receipt) => receipt,
        Err(_) => {
            return artifact_inspection(
                dir,
                id,
                "malformed receipt; raw metadata only, no original path is inferred",
            )
        }
    };
    let live = if receipt.path.is_empty() {
        None
    } else {
        cached_live_snapshot(tree, &receipt.path, cache)?
    };
    let retained = internal_snapshot(dir, &receipt.version)?;
    let token = hex(&Sha256::digest(serde_json::to_vec(&(
        &receipt, &live, &retained, id,
    ))?));
    let l = live.as_ref().map(|v| &v.fingerprint);
    let r = retained.as_ref().map(|v| &v.fingerprint);
    let explanation = match receipt.state {
        RecoveryState::Pending if l == receipt.expected.as_ref() && (r == receipt.replacement.as_ref() || r.is_none()) =>
            "publication may be staged or not started; live and saved versions require an owner choice",
        RecoveryState::Pending if l == receipt.replacement.as_ref() && r == receipt.expected.as_ref() =>
            "publication appears applied but completion was not recorded; review both versions",
        RecoveryState::Pending => "interrupted publication is ambiguous; no version is chosen automatically",
        RecoveryState::Discarding => "an explicit discard was interrupted; repeat discard with the recorded or current token",
        _ if r != receipt.expected.as_ref() => "retained inode changed after publication or review; data is preserved",
        _ => "retained version is available for owner inspection",
    }.to_owned();
    Ok(RecoveryInspection {
        id: id.into(),
        recorded: true,
        receipt,
        live,
        retained,
        token,
        explanation,
    })
}
/// Read-only owner snapshot. Capturing it requires only a short Store lock;
/// file inspection/export uses no Store lock and carries no mutation authority.
#[derive(Clone)]
pub struct RecoveryContext {
    root: PathBuf,
    body: String,
}
impl RecoveryContext {
    pub fn from_store(store: &Store) -> Self {
        Self {
            root: pin_root(store),
            body: store.body_name.clone(),
        }
    }
    pub fn list_page(&self, cursor: Option<&str>, limit: usize) -> Result<RecoveryList> {
        list_recovery_page(self, cursor, limit)
    }
    pub fn inspect(&self, id: &str) -> Result<RecoveryInspection> {
        let tree = Tree::open(&self.root)?;
        let (dir, _guard) = tree.recovery_lock()?;
        inspection(&tree, &dir, id)
    }
    pub fn export(&self, id: &str, which: RecoveryRead, token: &str) -> Result<RecoveryExport> {
        open_recovery_export(self, id, which, token)
    }
}
pub fn recovery_list(store: &Store) -> Result<RecoveryList> {
    recovery_list_page(store, None, MAX_RECOVERY_PAGE)
}
/// Sorted basename cursor, bounded memory even for pre-limit recovery data.
/// A page may be empty when all selected names belong to receipts on another
/// page. Continue until next_cursor is None. Usage covers the whole directory;
/// entry problems and `blocked` beyond admission cover this page only.
pub fn recovery_list_page(
    store: &Store,
    cursor: Option<&str>,
    limit: usize,
) -> Result<RecoveryList> {
    RecoveryContext::from_store(store).list_page(cursor, limit)
}
fn list_recovery_page(
    context: &RecoveryContext,
    cursor: Option<&str>,
    limit: usize,
) -> Result<RecoveryList> {
    if limit == 0 || limit > MAX_RECOVERY_PAGE {
        return Err(ClixError::Usage(
            "recovery page size must be 1 through 128".into(),
        ));
    }
    if let Some(cursor) = cursor {
        valid_recovery_name(cursor)?;
    }
    let tree = Tree::open(&context.root)?;
    let (dir, _guard) = tree.recovery_lock()?;
    let mut names = BTreeSet::new();
    let mut bytes = 0u64;
    let mut count = 0u64;
    for_each_recovery_name(&dir, |name| {
        let info =
            rustix::fs::statat(&dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).map_err(os)?;
        bytes = bytes.saturating_add(info.st_size.max(0) as u64);
        count = count.saturating_add(1);
        if cursor.is_none_or(|cursor| name > cursor) {
            names.insert(name.to_owned());
            if names.len() > limit + 1 {
                names.pop_last();
            }
        }
        Ok(())
    })?;
    let next_cursor = if names.len() > limit {
        names.pop_last();
        names.last().cloned()
    } else {
        None
    };
    let usage = usage_from_totals(bytes, count);
    // Discover ownership only for this bounded page's data files. Reading one
    // bounded receipt at a time permits legacy directories above admission.
    let mut linked = BTreeSet::new();
    for_each_recovery_name(&dir, |id| {
        if id.ends_with(".json") {
            if let Ok(receipt) =
                read_internal(&dir, id, MAX_RECEIPT).and_then(|b| parse_receipt(&b, id))
            {
                if names.contains(&receipt.version) {
                    linked.insert(receipt.version);
                }
            }
        }
        Ok(())
    })?;
    let mut entries = Vec::new();
    let mut cache = LiveCache::new();
    for id in names.iter().filter(|s| s.ends_with(".json")) {
        match inspection_cached(&tree, &dir, id, &mut cache) {
            Ok(view) if !view.recorded => entries.push(RecoveryEntry {
                id: id.clone(),
                path: None,
                state: "unreferenced".into(),
                problem: Some(view.explanation),
            }),
            Ok(view) => {
                let healthy = matches!(
                    view.receipt.state,
                    RecoveryState::Retained | RecoveryState::Resolved
                ) && view.retained.as_ref().map(|v| &v.fingerprint)
                    == view.receipt.expected.as_ref();
                entries.push(RecoveryEntry {
                    id: id.clone(),
                    path: Some(view.receipt.path),
                    state: serde_json::to_value(&view.receipt.state)?
                        .as_str()
                        .unwrap_or("unknown")
                        .into(),
                    problem: (!healthy).then_some(view.explanation),
                });
            }
            Err(e) => entries.push(RecoveryEntry {
                id: id.clone(),
                path: None,
                state: "invalid".into(),
                problem: Some(e.to_string()),
            }),
        }
    }
    // Check cached observations once more before presenting a healthy row.
    // This is a read-only observation, never a token used for mutation.
    for entry in &mut entries {
        if let Some(path) = &entry.path {
            if !path.is_empty() {
                if let Err(e) = cached_live_snapshot(&tree, path, &mut cache) {
                    entry.problem = Some(e.to_string());
                }
            }
        }
    }
    for id in names
        .into_iter()
        .filter(|id| !id.ends_with(".json") && !linked.contains(id))
    {
        entries.push(RecoveryEntry {
            id,
            path: None,
            state: "unreferenced".into(),
            problem: Some(
                "unreferenced recovery data; preserved, never automatically deleted".into(),
            ),
        });
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    let blocked = usage.admission_blocked || entries.iter().any(|r| r.problem.is_some());
    Ok(RecoveryList {
        body: context.body.clone(),
        usage,
        blocked,
        entries,
        next_cursor,
    })
}
pub fn recovery_inspect(store: &Store, id: &str) -> Result<RecoveryInspection> {
    RecoveryContext::from_store(store).inspect(id)
}
pub fn recovery_read(store: &Store, id: &str, which: RecoveryRead, token: &str) -> Result<Vec<u8>> {
    let tree = Tree::open(&pin_root(store))?;
    let (dir, _guard) = tree.recovery_lock()?;
    let before = inspection(&tree, &dir, id)?;
    if before.token != token {
        return Err(changed("recovery inspection is stale; inspect again"));
    }
    if !before.recorded && which != RecoveryRead::Retained {
        return Err(ClixError::Usage(
            "unreferenced data has only a retained version".into(),
        ));
    }
    let bytes = match which {
        RecoveryRead::Receipt => read_internal(&dir, id, MAX_RECEIPT)?,
        RecoveryRead::Retained => read_internal(&dir, &before.receipt.version, MAX_FILE)?,
        RecoveryRead::Live => {
            tree.read(&before.receipt.path)?
                .ok_or_else(|| ClixError::Usage("live version is absent".into()))?
                .1
        }
    };
    if inspection(&tree, &dir, id)?.token != token {
        return Err(changed("recovery version changed while reading"));
    }
    Ok(bytes)
}
pub fn recovery_resolve(
    store: &Store,
    id: &str,
    token: &str,
    choice: RecoveryChoice,
) -> Result<RecoveryInspection> {
    store.ensure_writable()?;
    let tree = Tree::open(&pin_root(store))?;
    let (dir, _guard) = tree.recovery_lock()?;
    let before = inspection(&tree, &dir, id)?;
    if before.token != token {
        return Err(changed("recovery inspection is stale; inspect again"));
    }
    if !before.recorded {
        return Err(ClixError::Usage(
            "unreferenced data has no destination; export it or explicitly discard it".into(),
        ));
    }
    if before.receipt.state == RecoveryState::Discarding {
        return Err(ClixError::Usage(
            "finish the interrupted discard before resolving this receipt".into(),
        ));
    }
    if choice == RecoveryChoice::RestoreRetained {
        let source = before
            .retained
            .as_ref()
            .ok_or_else(|| ClixError::Usage("there is no retained version to restore".into()))?;
        let bytes = read_internal(&dir, &before.receipt.version, MAX_FILE)?;
        if fingerprint(&bytes, source.fingerprint.mode) != source.fingerprint
            || inspection(&tree, &dir, id)?.token != token
        {
            return Err(changed("retained version changed before restoration"));
        }
        // Reuse publication's expected-version checks and displaced-inode
        // journal. If interrupted, BOTH receipts remain inspectable; an old
        // token never authorizes a second, different restoration.
        tree.apply_locked(
            &Change {
                path: before.receipt.path.clone(),
                expected: before.live.as_ref().map(|v| v.fingerprint.clone()),
                new: Some(source.fingerprint.clone()),
                to_remote: false,
            },
            Some(&bytes),
            store,
            &dir,
            &[id],
        )?;
    }
    let after = inspection(&tree, &dir, id)?;
    if after.receipt != before.receipt {
        return Err(changed("recovery receipt changed during resolution"));
    }
    if after.retained != before.retained {
        return Err(changed(
            "retained version changed during resolution; all versions were preserved",
        ));
    }
    if choice == RecoveryChoice::KeepLive && after.live != before.live {
        return Err(changed("live version changed during resolution"));
    }
    let retained = open_internal(&dir, &before.receipt.version)?;
    if let Some(file) = &retained {
        file.sync_all()?;
    }
    RecoveryExport::check_optional_file(retained, after.retained.as_ref())?;
    let mut receipt = before.receipt;
    receipt.expected = after.retained.as_ref().map(|v| v.fingerprint.clone());
    receipt.replacement = after.live.as_ref().map(|v| v.fingerprint.clone());
    receipt.state = RecoveryState::Resolved;
    write_typed_receipt(&dir, id, &receipt)?;
    inspection(&tree, &dir, id)
}
pub fn recovery_discard(store: &Store, id: &str, token: &str) -> Result<()> {
    store.ensure_writable()?;
    let tree = Tree::open(&pin_root(store))?;
    let (dir, _guard) = tree.recovery_lock()?;
    let view = inspection(&tree, &dir, id)?;
    let resume = view.receipt.state == RecoveryState::Discarding
        && view.receipt.discard_token.as_deref() == Some(token);
    if view.token != token && !resume {
        return Err(changed("recovery inspection is stale; inspect again"));
    }
    if view.receipt.state == RecoveryState::Pending {
        return Err(ClixError::Usage(
            "resolve the interrupted publication before discarding its retained version".into(),
        ));
    }
    if resume
        && view.retained.as_ref().map(|v| &v.fingerprint) != view.receipt.expected.as_ref()
        && view.retained.is_some()
    {
        return Err(changed(
            "retained version changed during interrupted discard",
        ));
    }
    let journal_id = if view.recorded {
        id.to_owned()
    } else {
        format!("{}.json", crate::job::new_id()?)
    };
    let mut receipt = view.receipt;
    receipt.state = RecoveryState::Discarding;
    receipt.expected = view.retained.as_ref().map(|v| v.fingerprint.clone());
    receipt.discard_token = Some(token.into());
    write_typed_receipt(&dir, &journal_id, &receipt)?;
    let current = internal_snapshot(&dir, &receipt.version)?;
    if current != view.retained {
        return Err(changed("retained version changed before discard"));
    }
    if current.is_some() {
        rustix::fs::unlinkat(&dir, &receipt.version, rustix::fs::AtFlags::empty()).map_err(os)?;
        dir.sync_all()?;
    }
    rustix::fs::unlinkat(&dir, &journal_id, rustix::fs::AtFlags::empty()).map_err(os)?;
    dir.sync_all()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinConflictSnapshot {
    pub peer: String,
    pub peer_key: String,
    pub path: String,
    pub local: Option<Fingerprint>,
    pub remote: Option<Fingerprint>,
    pub local_baseline: Option<Fingerprint>,
    pub remote_baseline: Option<Fingerprint>,
    pub token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PinConflictReport {
    pub peer: String,
    pub baseline_mismatch: bool,
    pub conflicts: Vec<PinConflictSnapshot>,
}
fn conflict_snapshot(
    peer: &Peer,
    path: String,
    local: Option<Fingerprint>,
    remote: Option<Fingerprint>,
    local_baseline: Option<Fingerprint>,
    remote_baseline: Option<Fingerprint>,
) -> Result<PinConflictSnapshot> {
    let peer_key = key(&peer.owner_pk);
    let token = hex(&Sha256::digest(serde_json::to_vec(&(
        &peer.name.0,
        &peer_key,
        &path,
        &local,
        &remote,
        &local_baseline,
        &remote_baseline,
    ))?));
    Ok(PinConflictSnapshot {
        peer: peer.name.0.clone(),
        peer_key,
        path,
        local,
        remote,
        local_baseline,
        remote_baseline,
        token,
    })
}
fn owner_peer(store: &Arc<Mutex<Store>>, name: &str) -> Result<(Peer, Vec<u8>, PathBuf, Manifest)> {
    let s = lock(store);
    ensure_enabled(&s)?;
    let peer = s
        .peers
        .iter()
        .find(|p| p.name.0 == name)
        .cloned()
        .ok_or_else(|| ClixError::Usage(format!("{name} is not paired")))?;
    Ok((
        peer.clone(),
        s.owner_sk.clone(),
        pin_root(&s),
        baseline(&s, &peer.owner_pk),
    ))
}
async fn peer_listing(peer: &Peer, sk: &[u8]) -> Result<Listing> {
    let value = crate::mesh::call(
        peer.addr.as_deref().ok_or(ClixError::Unreachable)?,
        sk,
        &peer.owner_pk,
        json!({"op":"pin_list"}),
    )
    .await?;
    let list: Listing = serde_json::from_value(value)?;
    validate_listing(&list)?;
    Ok(list)
}
fn validate_listing(list: &Listing) -> Result<()> {
    if list.files.len() > MAX_PIN_ENTRIES || list.baseline.len() > MAX_PIN_ENTRIES {
        return Err(ClixError::Protocol(
            "peer pin listing exceeds 4096 entries".into(),
        ));
    }
    for path in list.files.keys().chain(list.baseline.keys()) {
        valid_path(path)?;
    }
    Ok(())
}
pub async fn inspect_peer_conflicts(
    store: &Arc<Mutex<Store>>,
    name: &str,
) -> Result<PinConflictReport> {
    let (peer, sk, root, base) = owner_peer(store, name)?;
    let (_, local) = open_scan(root).await?;
    let remote = peer_listing(&peer, &sk).await?;
    let mismatch = base != remote.baseline;
    let paths: BTreeSet<String> = local
        .keys()
        .chain(remote.files.keys())
        .chain(base.keys())
        .chain(remote.baseline.keys())
        .cloned()
        .collect();
    let mut conflicts = Vec::new();
    for path in paths {
        let l = local.get(&path);
        let r = remote.files.get(&path);
        let a = base.get(&path);
        let b = remote.baseline.get(&path);
        if l != r && (a != b || (l != a && r != a)) {
            conflicts.push(conflict_snapshot(
                &peer,
                path.clone(),
                l.cloned(),
                r.cloned(),
                a.cloned(),
                b.cloned(),
            )?);
        }
    }
    Ok(PinConflictReport {
        peer: name.into(),
        baseline_mismatch: mismatch,
        conflicts,
    })
}
/// The owner accepts the inspected peer version ON THIS MACHINE. No remote
/// recovery decision or forced overwrite of the peer is sent over the mesh.
pub async fn take_peer_conflict(
    store: &Arc<Mutex<Store>>,
    selected: &PinConflictSnapshot,
) -> Result<()> {
    let refreshed = inspect_peer_conflicts(store, &selected.peer).await?;
    if !refreshed.conflicts.iter().any(|c| c == selected) {
        return Err(changed("pin conflict inspection is stale; inspect again"));
    }
    let (peer, sk, root, base) = owner_peer(store, &selected.peer)?;
    if key(&peer.owner_pk) != selected.peer_key
        || base.get(&selected.path) != selected.local_baseline.as_ref()
    {
        return Err(changed("pin peer or baseline changed"));
    }
    let bytes = if let Some(expected) = &selected.remote {
        let value = crate::mesh::call(
            peer.addr.as_deref().ok_or(ClixError::Unreachable)?,
            &sk,
            &peer.owner_pk,
            json!({"op":"pin_get","path":selected.path,"expected":expected}),
        )
        .await?;
        Some(unhex(value["content"].as_str().ok_or_else(|| {
            ClixError::Protocol("missing pin content".into())
        })?)?)
    } else {
        None
    };
    let store = store.clone();
    let selected = selected.clone();
    blocking(move || {
        let s = lock(&store);
        current_sync_authority(&s, &peer, &root)?;
        if baseline(&s, &peer.owner_pk).get(&selected.path) != selected.local_baseline.as_ref() {
            return Err(changed("pin peer or baseline changed"));
        }
        Tree::open(&root)?.apply(
            &Change {
                path: selected.path,
                expected: selected.local,
                new: selected.remote,
                to_remote: false,
            },
            bytes.as_deref(),
            &s,
        )
    })
    .await
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PinSyncStatus {
    pub peer: String,
    pub synced: bool,
    pub error: Option<String>,
}
pub async fn owner_pin_sync(store: &Arc<Mutex<Store>>, name: &str) -> Result<PinSyncStatus> {
    let (peer, sk, _, _) = owner_peer(store, name)?;
    let outcome = sync_with_peer(
        store,
        &sk,
        peer.addr.as_deref().ok_or(ClixError::Unreachable)?,
        name,
    )
    .await;
    match outcome {
        Ok(()) => Ok(PinSyncStatus {
            peer: name.into(),
            synced: true,
            error: None,
        }),
        Err(e) => {
            let message = e.to_string();
            lock(store).update(|s| {
                s.pin_index.last_error = Some(message.clone());
                Ok(())
            })?;
            Ok(PinSyncStatus {
                peer: name.into(),
                synced: false,
                error: Some(message),
            })
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryChunk {
    pub offset: u64,
    pub total_bytes: u64,
    pub bytes: Vec<u8>,
    /// True only after the complete stream's hash and version checks pass.
    pub eof: bool,
}

/// One owner export, holding the pin lock and source descriptor independently
/// of Store. The caller must release its Store mutex before streaming, impose
/// connection deadlines, and report success only after `finish()` succeeds.
/// There is no global session cache and dropping this object releases the lock.
pub struct RecoveryExport {
    tree: Tree,
    dir: File,
    _guard: RecoveryLock,
    source: File,
    expected: RecoveryVersion,
    view: RecoveryInspection,
    receipt: Option<RecoveryVersion>,
    recovery_metadata: fs::Metadata,
    offset: u64,
    hash: Sha256,
    complete: bool,
    failed: bool,
}

pub fn recovery_export(
    store: &Store,
    id: &str,
    which: RecoveryRead,
    token: &str,
) -> Result<RecoveryExport> {
    RecoveryContext::from_store(store).export(id, which, token)
}
fn open_recovery_export(
    context: &RecoveryContext,
    id: &str,
    which: RecoveryRead,
    token: &str,
) -> Result<RecoveryExport> {
    let tree = Tree::open(&context.root)?;
    let (dir, guard) = tree.recovery_lock()?;
    // Hash each inspected version only once. Subsequent checks compare opened
    // inode identities and metadata; the selected stream is hashed as it leaves.
    let view = inspection(&tree, &dir, id)?;
    if view.token != token {
        return Err(changed("recovery inspection is stale; inspect again"));
    }
    if !view.recorded && which != RecoveryRead::Retained {
        return Err(ClixError::Usage(
            "unreferenced data has only a retained version".into(),
        ));
    }
    let mut receipt_file = None;
    let receipt = if view.recorded {
        let mut file =
            open_internal(&dir, id)?.ok_or_else(|| changed("recovery receipt disappeared"))?;
        let before = file.metadata()?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_RECEIPT + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if bytes.len() as u64 > MAX_RECEIPT
            || !same_metadata(&before, &after)
            || parse_receipt(&bytes, id)? != view.receipt
        {
            return Err(changed("recovery receipt changed while opening export"));
        }
        let version = version_from_metadata(&after, hex(&Sha256::digest(&bytes)));
        file.seek(SeekFrom::Start(0))?;
        receipt_file = Some(file);
        Some(version)
    } else {
        None
    };
    let (source, expected) = match which {
        RecoveryRead::Receipt => (
            receipt_file.ok_or_else(|| ClixError::Usage("recovery receipt is absent".into()))?,
            receipt
                .clone()
                .ok_or_else(|| ClixError::Usage("recovery receipt is absent".into()))?,
        ),
        RecoveryRead::Retained => (
            open_internal(&dir, &view.receipt.version)?
                .ok_or_else(|| ClixError::Usage("retained version is absent".into()))?,
            view.retained
                .clone()
                .ok_or_else(|| ClixError::Usage("retained version is absent".into()))?,
        ),
        RecoveryRead::Live => (
            open_live(&tree, &view.receipt.path)?
                .ok_or_else(|| ClixError::Usage("live version is absent".into()))?,
            view.live
                .clone()
                .ok_or_else(|| ClixError::Usage("live version is absent".into()))?,
        ),
    };
    let recovery_metadata = dir.metadata()?;
    let export = RecoveryExport {
        tree,
        dir,
        _guard: guard,
        source,
        expected,
        view,
        receipt,
        recovery_metadata,
        offset: 0,
        hash: Sha256::new(),
        complete: false,
        failed: false,
    };
    export.check_versions()?;
    Ok(export)
}
impl RecoveryExport {
    pub fn total_bytes(&self) -> u64 {
        self.expected.bytes
    }

    fn check_versions(&self) -> Result<()> {
        let current_root = fs::symlink_metadata(&self.tree.path)?;
        let opened_root = self.tree.root.metadata()?;
        if !current_root.is_dir()
            || current_root.dev() != opened_root.dev()
            || current_root.ino() != opened_root.ino()
        {
            return Err(changed("pin root moved during recovery export"));
        }
        let current_dir = self
            .tree
            .recovery_dir(false)?
            .ok_or_else(|| changed("recovery directory disappeared during export"))?;
        if !same_metadata(&current_dir.metadata()?, &self.recovery_metadata)
            || !same_metadata(&self.dir.metadata()?, &self.recovery_metadata)
            || !matches_version(&self.source.metadata()?, &self.expected)
        {
            return Err(changed("recovery version changed during export"));
        }
        if self.view.recorded {
            Self::check_optional_file(
                open_internal(&self.dir, &self.view.id)?,
                self.receipt.as_ref(),
            )?;
        }
        if !self.view.receipt.path.is_empty() {
            Self::check_optional_file(
                open_live(&self.tree, &self.view.receipt.path)?,
                self.view.live.as_ref(),
            )?;
        }
        Self::check_optional_file(
            open_internal(&self.dir, &self.view.receipt.version)?,
            self.view.retained.as_ref(),
        )
    }
    fn check_optional_file(file: Option<File>, expected: Option<&RecoveryVersion>) -> Result<()> {
        let valid = match (file, expected) {
            (Some(file), Some(version)) => matches_version(&file.metadata()?, version),
            (None, None) => true,
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(changed("inspected version changed during recovery export"))
        }
    }
    pub fn read_chunk(&mut self, length: usize) -> Result<RecoveryChunk> {
        if self.failed {
            return Err(changed("recovery export already failed; inspect again"));
        }
        let result = self.read_chunk_checked(length);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn read_chunk_checked(&mut self, length: usize) -> Result<RecoveryChunk> {
        if length == 0 || length > 65536 || self.complete {
            return Err(ClixError::Usage(
                "recovery chunks require 1 through 65536 bytes and an unfinished export".into(),
            ));
        }
        self.check_versions()?;
        let offset = self.offset;
        let mut bytes = vec![0; (self.expected.bytes - offset).min(length as u64) as usize];
        self.source.read_exact(&mut bytes)?;
        self.check_versions()?;
        self.hash.update(&bytes);
        self.offset += bytes.len() as u64;
        let eof = self.offset == self.expected.bytes;
        if eof {
            if hex(&self.hash.clone().finalize()) != self.expected.fingerprint.hash {
                return Err(changed("exported content did not match the inspected hash"));
            }
            self.check_versions()?;
            self.complete = true;
        }
        Ok(RecoveryChunk {
            offset,
            total_bytes: self.expected.bytes,
            bytes,
            eof,
        })
    }
    /// The owner RPC must call this before sending its terminal success frame.
    /// A dropped or partially consumed stream has never completed successfully.
    pub fn finish(&mut self) -> Result<()> {
        if self.failed || !self.complete {
            return Err(changed("recovery export is incomplete or failed"));
        }
        let result = self.check_versions();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
