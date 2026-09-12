use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{ClixError, Result};
use crate::store::{PinIndex, Store};
use crate::types::Peer;

struct FileMeta {
    hash: String,
    mtime: SystemTime,
}

/// Default `~/src`, override `CLIX_PIN`.
pub fn pin_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLIX_PIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    PathBuf::from(home).join("src")
}

pub fn pin_root(store: &Store) -> PathBuf {
    store
        .pin_dir
        .clone()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(pin_dir)
}

/// Two-way copy between local pin trees. Conflict if both wrote since last sync.
/// Both stores record the same last-sync timestamp on success.
pub fn sync(
    local_store: &mut Store,
    local: &Path,
    remote_store: &mut Store,
    remote: &Path,
) -> Result<()> {
    let local_files = scan(local)?;
    let remote_files = scan(remote)?;
    let local_name = body_name(local_store);
    let remote_name = body_name(remote_store);
    let (to_remote, to_local) = plan(
        &local_files,
        &remote_files,
        &local_store.pin_index,
        &local_name,
        &remote_name,
    )?;
    for rel in &to_remote {
        copy_rel(local, remote, rel)?;
    }
    for rel in &to_local {
        copy_rel(remote, local, rel)?;
    }
    let last_sync = now_millis();
    let local_synced = scan(local)?;
    let remote_synced = scan(remote)?;
    record_index(local_store, &local_synced, last_sync)?;
    record_index(remote_store, &remote_synced, last_sync)
}

pub async fn sync_with_peers(store: Arc<Mutex<Store>>) -> Result<()> {
    let (peers, sk) = {
        let s = lock_store(&store);
        (s.peers.clone(), s.owner_sk.clone())
    };
    for peer in peers {
        let Some(addr) = peer.addr.clone() else {
            continue;
        };
        sync_with_peer(&store, &sk, &addr, &peer.name.0).await?;
    }
    Ok(())
}

pub async fn sync_with_peer(
    store: &Arc<Mutex<Store>>,
    owner_sk: &[u8],
    addr: &str,
    peer_name: &str,
) -> Result<()> {
    match sync_with_peer_inner(store, owner_sk, addr, peer_name).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if is_conflict(&e) {
                remember_conflict(store, &e);
            }
            Err(e)
        }
    }
}

async fn sync_with_peer_inner(
    store: &Arc<Mutex<Store>>,
    owner_sk: &[u8],
    addr: &str,
    peer_name: &str,
) -> Result<()> {
    let local_root = {
        let s = lock_store(store);
        pin_root(&s)
    };
    let local_files = scan(&local_root)?;
    let list = crate::mesh::call(addr, owner_sk, json!({"op": "pin_list"})).await?;
    let remote_files = files_from_list(&list)?;
    let (to_remote, to_local) = {
        let s = lock_store(store);
        let local_name = body_name(&s);
        plan(
            &local_files,
            &remote_files,
            &s.pin_index,
            &local_name,
            peer_name,
        )?
    };
    for rel in &to_remote {
        let bytes = fs::read(safe_join(&local_root, rel)?)?;
        let meta = local_files
            .get(rel)
            .ok_or_else(|| ClixError::Io(format!("pin missing {rel}")))?;
        crate::mesh::call(
            addr,
            owner_sk,
            json!({
                "op": "pin_put",
                "path": rel,
                "hash": meta.hash,
                "mtime": mtime_millis(meta.mtime),
                "content": hex_encode(&bytes),
            }),
        )
        .await?;
    }
    for rel in &to_local {
        let v = crate::mesh::call(addr, owner_sk, json!({"op": "pin_get", "path": rel})).await?;
        let content = hex_decode(v.get("content").and_then(Value::as_str).unwrap_or(""))?;
        let dest = safe_join(&local_root, rel)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, content)?;
        if let Some(ms) = v.get("mtime").and_then(Value::as_u64) {
            set_mtime(&dest, millis_to_time(ms))?;
        }
    }
    let last_sync = now_millis();
    crate::mesh::call(
        addr,
        owner_sk,
        json!({"op": "pin_commit", "last_sync": last_sync}),
    )
    .await?;
    let synced = scan(&local_root)?;
    let mut s = lock_store(store);
    record_index(&mut s, &synced, last_sync)
}

/// After pair, 401/unknown peer is the only pin failure mapped to Ok (the
/// other sidecar may not have persisted us yet). I/O, decode, and conflict
/// still surface.
pub async fn sync_after_pair(store: &Arc<Mutex<Store>>, sk: &[u8], peer: &Peer) -> Result<()> {
    let Some(addr) = peer.addr.as_deref() else {
        return Ok(());
    };
    match sync_with_peer(store, sk, addr, &peer.name.0).await {
        Ok(()) => Ok(()),
        Err(e) if is_unknown_peer(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

pub(crate) fn rpc_list(store: &Store) -> Result<Value> {
    let files: Vec<Value> = scan(&pin_root(store))?
        .into_iter()
        .map(|(path, meta)| {
            json!({
                "path": path,
                "hash": meta.hash,
                "mtime": mtime_millis(meta.mtime),
            })
        })
        .collect();
    Ok(json!({"ok": true, "files": files}))
}

pub(crate) fn rpc_get(store: &Store, req: &Value) -> Result<Value> {
    let rel = req.get("path").and_then(Value::as_str).unwrap_or("");
    let path = safe_join(&pin_root(store), rel)?;
    let bytes = fs::read(&path)?;
    let mtime = fs::metadata(&path)?.modified().unwrap_or(UNIX_EPOCH);
    Ok(json!({
        "ok": true,
        "path": rel,
        "hash": hash_bytes(&bytes),
        "mtime": mtime_millis(mtime),
        "content": hex_encode(&bytes),
    }))
}

pub(crate) fn rpc_put(store: &Store, req: &Value) -> Result<Value> {
    let rel = req.get("path").and_then(Value::as_str).unwrap_or("");
    let content = hex_decode(req.get("content").and_then(Value::as_str).unwrap_or(""))?;
    let dest = safe_join(&pin_root(store), rel)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&dest, content)?;
    if let Some(ms) = req.get("mtime").and_then(Value::as_u64) {
        set_mtime(&dest, millis_to_time(ms))?;
    }
    Ok(json!({"ok": true}))
}

pub(crate) fn rpc_commit(store: &mut Store, req: &Value) -> Result<Value> {
    let last_sync = req
        .get("last_sync")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClixError::Io("pin_commit missing last_sync".into()))?;
    let files = scan(&pin_root(store))?;
    record_index(store, &files, last_sync)?;
    Ok(json!({"ok": true}))
}

pub(crate) fn is_conflict(e: &ClixError) -> bool {
    matches!(e, ClixError::PinConflict { .. }) || e.to_string().contains("Not merging")
}

fn is_unknown_peer(e: &ClixError) -> bool {
    e.to_string().contains("unknown peer")
}

fn remember_conflict(store: &Arc<Mutex<Store>>, e: &ClixError) {
    let mut s = lock_store(store);
    s.pin_index.last_error = Some(e.to_string());
    let _ = s.save();
}

fn body_name(store: &Store) -> String {
    if store.body_name.is_empty() {
        "this".into()
    } else {
        store.body_name.clone()
    }
}

fn plan(
    local: &BTreeMap<String, FileMeta>,
    remote: &BTreeMap<String, FileMeta>,
    index: &PinIndex,
    local_name: &str,
    remote_name: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut paths: Vec<&str> = local
        .keys()
        .chain(remote.keys())
        .map(|s| s.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let mut to_remote = Vec::new();
    let mut to_local = Vec::new();
    for path in paths {
        match (local.get(path), remote.get(path)) {
            (Some(_), None) => to_remote.push(path.to_string()),
            (None, Some(_)) => to_local.push(path.to_string()),
            (Some(l), Some(r)) if l.hash == r.hash => {}
            (Some(l), Some(r)) => {
                let l_new = changed_since(l.mtime, index.last_sync);
                let r_new = changed_since(r.mtime, index.last_sync);
                // Both wrote since last successful sync: stop. Do not invent a merge.
                if l_new && r_new {
                    return Err(conflict(path, local_name, remote_name));
                }
                if l_new {
                    to_remote.push(path.to_string());
                } else if r_new {
                    to_local.push(path.to_string());
                } else {
                    return Err(conflict(path, local_name, remote_name));
                }
            }
            (None, None) => {}
        }
    }
    Ok((to_remote, to_local))
}

fn conflict(path: &str, a: &str, b: &str) -> ClixError {
    ClixError::PinConflict {
        path: format!("src/{path}"),
        a: a.to_string(),
        b: b.to_string(),
    }
}

fn changed_since(mtime: SystemTime, last_sync: Option<u64>) -> bool {
    match last_sync {
        None => true,
        Some(ms) => mtime_millis(mtime) > ms,
    }
}

fn scan(root: &Path) -> Result<BTreeMap<String, FileMeta>> {
    let mut out = BTreeMap::new();
    if !root.exists() {
        return Ok(out);
    }
    scan_dir(root, root, &mut out)?;
    Ok(out)
}

fn scan_dir(root: &Path, dir: &Path, out: &mut BTreeMap<String, FileMeta>) -> Result<()> {
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        let path = ent.path();
        let ft = ent.file_type()?;
        if ft.is_dir() {
            scan_dir(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.is_empty() {
                continue;
            }
            let hash = hash_file(&path)?;
            let mtime = fs::metadata(&path)?.modified().unwrap_or(UNIX_EPOCH);
            out.insert(rel, FileMeta { hash, mtime });
        }
    }
    Ok(())
}

fn copy_rel(from: &Path, to: &Path, rel: &str) -> Result<()> {
    let src = safe_join(from, rel)?;
    let dst = safe_join(to, rel)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&src, &dst)?;
    if let Ok(mtime) = fs::metadata(&src).and_then(|m| m.modified()) {
        set_mtime(&dst, mtime)?;
    }
    Ok(())
}

fn set_mtime(path: &Path, t: SystemTime) -> Result<()> {
    fs::File::open(path)?.set_modified(t)?;
    Ok(())
}

fn millis_to_time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

fn record_index(
    store: &mut Store,
    files: &BTreeMap<String, FileMeta>,
    last_sync: u64,
) -> Result<()> {
    store.pin_index.last_sync = Some(last_sync);
    store.pin_index.hashes = files
        .iter()
        .map(|(k, v)| (k.clone(), v.hash.clone()))
        .collect();
    store.pin_index.last_error = None;
    store.save()
}

fn files_from_list(v: &Value) -> Result<BTreeMap<String, FileMeta>> {
    let mut out = BTreeMap::new();
    let Some(arr) = v.get("files").and_then(Value::as_array) else {
        return Ok(out);
    };
    for f in arr {
        let path = f.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            continue;
        }
        let hash = f
            .get("hash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mtime = f.get("mtime").and_then(Value::as_u64).unwrap_or(0);
        out.insert(
            path.to_string(),
            FileMeta {
                hash,
                mtime: UNIX_EPOCH + Duration::from_millis(mtime),
            },
        );
    }
    Ok(out)
}

fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty()
        || Path::new(rel).is_absolute()
        || rel.split(['/', '\\']).any(|s| s == ".." || s.is_empty())
    {
        return Err(ClixError::Io("invalid pin path".into()));
    }
    Ok(root.join(rel))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut f = fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hash_bytes(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(ClixError::Io("invalid pin content".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| ClixError::Io("invalid pin content".into()))
        })
        .collect()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mtime_millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn lock_store(store: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}
