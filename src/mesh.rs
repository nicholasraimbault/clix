use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Notify};

use crate::error::{ClixError, Result};
use crate::exec::{exec_checked, exec_with_job};
use crate::job;
use crate::pair;
use crate::request;
use crate::store::Store;
use crate::types::{Job, Peer};

type HmacSha256 = Hmac<Sha256>;

const OWNER_OPS: &[&str] = &["add", "remove", "allow", "deny", "pair"];
const MAX_BODY: usize = 32 * 1024 * 1024;

/// Shared handle for pairing and mesh exec.
#[derive(Clone)]
pub struct MeshHandle {
    pub addr: String,
    inner: Arc<Mutex<MeshInner>>,
    pub woke: Arc<Notify>,
}

struct MeshInner {
    pair_tx: Option<oneshot::Sender<TcpStream>>,
    pair_done: Option<oneshot::Receiver<Result<Peer>>>,
}

/// TCP listener. Production binds Tailscale IPv4 `:7421` (or `CLIX_PORT`).
pub struct MeshListener {
    listener: TcpListener,
    handle: MeshHandle,
}

/// Production: this body's Tailscale IPv4 and `CLIX_PORT` (default 7421).
/// Tests set `CLIX_MESH_BIND` (e.g. `127.0.0.1:0`).
pub fn listen_addr() -> Result<String> {
    if let Ok(bind) = std::env::var("CLIX_MESH_BIND") {
        if !bind.is_empty() {
            return Ok(bind);
        }
    }
    crate::tailscale::mesh_bind_addr()
}

impl MeshListener {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        Ok(Self {
            listener,
            handle: MeshHandle {
                addr: local.to_string(),
                inner: Arc::new(Mutex::new(MeshInner {
                    pair_tx: None,
                    pair_done: None,
                })),
                woke: Arc::new(Notify::new()),
            },
        })
    }

    pub fn handle(&self) -> MeshHandle {
        self.handle.clone()
    }

    pub fn local_addr(&self) -> &str {
        &self.handle.addr
    }

    pub async fn run(&self, store: Arc<Mutex<Store>>) -> Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let handle = self.handle.clone();
            let store = store.clone();
            tokio::spawn(async move {
                let _ = serve_conn(store, handle, stream).await;
            });
        }
    }
}

impl MeshHandle {
    /// Register so the next accepted TCP stream is delivered for pairing.
    pub fn register_pair(&self) -> oneshot::Receiver<TcpStream> {
        let (tx, rx) = oneshot::channel();
        lock_inner(self).pair_tx = Some(tx);
        rx
    }

    pub fn set_pair_done(&self, rx: oneshot::Receiver<Result<Peer>>) {
        lock_inner(self).pair_done = Some(rx);
    }

    pub fn take_pair_done(&self) -> Result<oneshot::Receiver<Result<Peer>>> {
        lock_inner(self)
            .pair_done
            .take()
            .ok_or_else(|| ClixError::Usage("not pairing".into()))
    }

    pub fn wake(&self) {
        self.woke.notify_waiters();
    }
}

fn lock_inner(handle: &MeshHandle) -> std::sync::MutexGuard<'_, MeshInner> {
    handle.inner.lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_store(store: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

pub async fn dial(addr: &str) -> Result<TcpStream> {
    Ok(TcpStream::connect(addr).await?)
}

pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| ClixError::Io("frame too large".into()))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut lenb = [0u8; 4];
    stream.read_exact(&mut lenb).await?;
    let len = u32::from_be_bytes(lenb) as usize;
    if len > MAX_BODY {
        return Err(ClixError::Io("frame too large".into()));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

/// POST JSON to a paired body's mesh listener, signed with this body's owner key.
pub async fn call(addr: &str, owner_sk: &[u8], req: Value) -> Result<Value> {
    let body = serde_json::to_vec(&req)?;
    let pk = pair::owner_pk(owner_sk)?;
    let mac = mesh_mac(&pk, &body)?;
    let sig = sign_body(owner_sk, &body)?;
    let mut stream = TcpStream::connect(addr).await?;
    let head = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Clix-Pk: {}\r\nX-Clix-Mac: {}\r\nX-Clix-Sig: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        hex_encode(&pk),
        hex_encode(&mac),
        hex_encode(&sig),
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    let msg = read_http(&mut stream).await?;
    let v: Value = if msg.body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&msg.body)?
    };
    let status = msg.status().unwrap_or(0);
    if status != 200 || v.get("ok") == Some(&Value::Bool(false)) {
        let err = v
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| match status {
                401 => "unknown peer".into(),
                0 => "empty mesh response".into(),
                n => format!("mesh http {n}"),
            });
        return Err(ClixError::Io(err));
    }
    Ok(v)
}

async fn serve_conn(
    store: Arc<Mutex<Store>>,
    handle: MeshHandle,
    mut stream: TcpStream,
) -> Result<()> {
    let _ = stream.readable().await;
    let mut peek = [0u8; 4];
    let n = stream.peek(&mut peek).await.unwrap_or(0);
    if n >= 4 && looks_http(&peek) {
        handle_http(store, &handle, &mut stream).await
    } else {
        let waiter = {
            let mut inner = lock_inner(&handle);
            inner.pair_tx.take()
        };
        if let Some(tx) = waiter {
            let _ = tx.send(stream);
        }
        Ok(())
    }
}

fn looks_http(b: &[u8]) -> bool {
    b.starts_with(b"POST")
        || b.starts_with(b"GET ")
        || b.starts_with(b"PUT ")
        || b.starts_with(b"HEAD")
}

async fn handle_http(
    store: Arc<Mutex<Store>>,
    handle: &MeshHandle,
    stream: &mut TcpStream,
) -> Result<()> {
    let msg = read_http(stream).await?;
    let (status, v) = match dispatch_http(&store, handle, &msg).await {
        Ok(v) => (200, v),
        Err(e) => {
            let status = match &e {
                ClixError::Io(s) if s.contains("unknown") => 401,
                ClixError::PinConflict { .. } => 409,
                _ => 403,
            };
            (status, json!({"ok": false, "error": e.to_string()}))
        }
    };
    stream.write_all(&http_json(status, &v)?).await?;
    stream.flush().await?;
    Ok(())
}

async fn dispatch_http(
    store: &Arc<Mutex<Store>>,
    handle: &MeshHandle,
    msg: &HttpMsg,
) -> Result<Value> {
    if msg.method() != "POST" {
        return Err(ClixError::Usage("POST only".into()));
    }
    let pk = hex_decode(
        msg.header("x-clix-pk")
            .ok_or_else(|| ClixError::Io("unknown peer".into()))?,
    )?;
    let mac = hex_decode(
        msg.header("x-clix-mac")
            .ok_or_else(|| ClixError::Io("unknown peer".into()))?,
    )?;
    let sig = hex_decode(
        msg.header("x-clix-sig")
            .ok_or_else(|| ClixError::Io("unknown peer".into()))?,
    )?;
    let peer = {
        let store = lock_store(store);
        store
            .peers
            .iter()
            .find(|p| p.owner_pk == pk)
            .cloned()
            .ok_or_else(|| ClixError::Io("unknown peer".into()))?
    };
    if mesh_mac(&pk, &msg.body)? != mac || !verify_body(&pk, &msg.body, &sig) {
        return Err(ClixError::Io("unknown peer".into()));
    }
    let req: Value = serde_json::from_slice(&msg.body)?;
    handle_mesh_req(store, handle, &peer, req).await
}

async fn handle_mesh_req(
    store: &Arc<Mutex<Store>>,
    handle: &MeshHandle,
    peer: &Peer,
    req: Value,
) -> Result<Value> {
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    if OWNER_OPS.contains(&op) {
        return Err(ClixError::Usage(format!(
            "{op} is not allowed over the mesh"
        )));
    }
    match op {
        "exec" => {
            pin_before_exec(store, peer).await?;
            let argv = json_string_list(req.get("argv"));
            let job_id = req
                .get("job_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            match job_id {
                Some(id) => exec_with_job(store, &peer.name, &argv, Some(id)),
                None => exec_checked(store, &peer.name, &argv),
            }
        }
        "job_poll" => rpc_job_poll(store, handle, peer, &req),
        "job_result" => rpc_job_result(store, handle, peer, &req),
        "request" => rpc_request(store, peer, &req),
        "pin_list" => {
            let s = lock_store(store);
            crate::pin::rpc_list(&s)
        }
        "pin_get" => {
            let s = lock_store(store);
            crate::pin::rpc_get(&s, &req)
        }
        "pin_put" => {
            let s = lock_store(store);
            crate::pin::rpc_put(&s, &req)
        }
        "pin_commit" => {
            let mut s = lock_store(store);
            crate::pin::rpc_commit(&mut s, &req)
        }
        "" => Err(ClixError::Usage("missing op".into())),
        other => Err(ClixError::Usage(format!("unknown op: {other}"))),
    }
}

async fn pin_before_exec(store: &Arc<Mutex<Store>>, peer: &Peer) -> Result<()> {
    let Some(addr) = peer.addr.clone() else {
        return Ok(());
    };
    let sk = lock_store(store).owner_sk.clone();
    crate::pin::sync_with_peer(store, &sk, &addr, &peer.name.0).await
}

fn rpc_request(store: &Arc<Mutex<Store>>, peer: &Peer, req: &Value) -> Result<Value> {
    let tool = req.get("tool").and_then(Value::as_str).unwrap_or("");
    if tool.is_empty() {
        return Ok(json!({"ok": true}));
    }
    let mut s = lock_store(store);
    let r = request::upsert(&mut s, peer.name.clone(), tool)?;
    s.save()?;
    Ok(json!({"ok": true, "request": r}))
}

fn rpc_job_poll(
    store: &Arc<Mutex<Store>>,
    handle: &MeshHandle,
    peer: &Peer,
    req: &Value,
) -> Result<Value> {
    let jobs = {
        let mut s = lock_store(store);
        let mut changed = false;
        if let Some(addr) = req.get("addr").and_then(Value::as_str) {
            if !addr.is_empty() {
                if let Some(p) = s.peers.iter_mut().find(|p| p.owner_pk == peer.owner_pk) {
                    if p.addr.as_deref() != Some(addr) {
                        p.addr = Some(addr.to_string());
                        changed = true;
                    }
                }
            }
        }
        let jobs = s.claim_waiting_for(&peer.name);
        if changed || !jobs.is_empty() {
            s.save()?;
        }
        jobs
    };
    handle.wake();
    Ok(json!({"ok": true, "jobs": jobs}))
}

fn rpc_job_result(
    store: &Arc<Mutex<Store>>,
    handle: &MeshHandle,
    peer: &Peer,
    req: &Value,
) -> Result<Value> {
    let v = req
        .get("result")
        .and_then(|r| r.get("job"))
        .cloned()
        .or_else(|| req.get("job").cloned());
    if let Some(v) = v {
        let incoming: Job = serde_json::from_value(v)?;
        job::apply_peer_result(store, &peer.name, &incoming)?;
        handle.wake();
    }
    Ok(json!({"ok": true}))
}

/// On sidecar start: pin ~/src, tell peers our addr, pull waiting jobs destined here, run them.
pub async fn claim_waiting_jobs(store: Arc<Mutex<Store>>, local_addr: String) {
    if let Err(e) = crate::pin::sync_with_peers(store.clone()).await {
        eprintln!("{e}");
        return;
    }
    let (peers, sk, name) = {
        let s = lock_store(&store);
        (s.peers.clone(), s.owner_sk.clone(), s.body_name.clone())
    };
    for peer in peers {
        let Some(addr) = peer.addr.clone() else {
            continue;
        };
        let Ok(v) = call(&addr, &sk, json!({"op": "job_poll", "addr": local_addr})).await else {
            continue;
        };
        let jobs = v
            .get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for jv in jobs {
            let Ok(job) = serde_json::from_value::<Job>(jv) else {
                continue;
            };
            if job.body.0 != name {
                continue;
            }
            let Ok(result) = exec_with_job(&store, &peer.name, &job.argv, Some(job.id.clone()))
            else {
                continue;
            };
            let st = result.get("status").and_then(Value::as_str);
            if st == Some("done") || st == Some("denied") || st == Some("failed") {
                let _ = call(&addr, &sk, json!({"op": "job_result", "result": result})).await;
            }
        }
    }
}

fn json_string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn mesh_mac(owner_pk: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    let mut mac =
        HmacSha256::new_from_slice(owner_pk).map_err(|_| ClixError::Io("mesh hmac key".into()))?;
    mac.update(body);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn sign_body(sk: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    let bytes: [u8; 32] = sk
        .try_into()
        .map_err(|_| ClixError::Io("owner key is not a valid ed25519 secret".into()))?;
    let sk = SigningKey::from_bytes(&bytes);
    Ok(sk.sign(body).to_bytes().to_vec())
}

fn verify_body(pk: &[u8], body: &[u8], sig: &[u8]) -> bool {
    let pk: [u8; 32] = match pk.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig: [u8; 64] = match sig.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    vk.verify(body, &Signature::from_bytes(&sig)).is_ok()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(ClixError::Io("unknown peer".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ClixError::Io("unknown peer".into()))
        })
        .collect()
}

struct HttpMsg {
    start: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpMsg {
    fn method(&self) -> &str {
        self.start.split_whitespace().next().unwrap_or("")
    }

    fn status(&self) -> Option<u16> {
        self.start.split_whitespace().nth(1)?.parse().ok()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn read_http(stream: &mut TcpStream) -> Result<HttpMsg> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = find_double_crlf(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return Err(ClixError::Io("http header too large".into()));
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(ClixError::Io("empty mesh response".into()));
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]);
    let rest = buf[header_end + 4..].to_vec();
    let mut lines = header_text.split("\r\n");
    let start = lines.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let content_len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_len > MAX_BODY {
        return Err(ClixError::Io("frame too large".into()));
    }
    let mut body = rest;
    while body.len() < content_len {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len);
    Ok(HttpMsg {
        start,
        headers,
        body,
    })
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn http_json(status: u16, v: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(v)?;
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(&body);
    Ok(out)
}
