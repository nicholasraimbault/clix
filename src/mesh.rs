use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Notify};

use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::Peer;
use crate::{exec, job, transport};

const MAX_FRAME: usize = crate::limits::MESH_FRAME_BYTES;
const IO_TIMEOUT: Duration = crate::limits::IO_TIMEOUT;
const PAIR_PREFACE: &[u8; 5] = b"CLXP1";
const TLS_PREFACE: &[u8; 5] = b"CLXT1";

#[derive(Clone)]
pub struct MeshHandle {
    inner: Arc<Mutex<MeshInner>>,
    pub woke: Arc<Notify>,
    pub(crate) limits: Arc<crate::limits::Limits>,
}
struct MeshInner {
    addr: String,
    pair_tx: Option<oneshot::Sender<TcpStream>>,
    pair_done: Option<oneshot::Receiver<Result<Peer>>>,
}
pub struct MeshListener {
    listener: TcpListener,
    handle: MeshHandle,
}

pub fn listen_addr() -> Result<String> {
    if let Ok(bind) = std::env::var("CLIX_MESH_BIND") {
        if !bind.is_empty() {
            return Ok(bind);
        }
    }
    crate::tailscale::mesh_bind_addr()
}
impl Default for MeshHandle {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MeshInner {
                addr: String::new(),
                pair_tx: None,
                pair_done: None,
            })),
            woke: Arc::new(Notify::new()),
            limits: Arc::new(crate::limits::Limits::default()),
        }
    }
}
impl MeshHandle {
    pub fn addr(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .addr
            .clone()
    }
    fn set_addr(&self, addr: String) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).addr = addr;
    }
    pub fn register_pair(&self) -> oneshot::Receiver<TcpStream> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).pair_tx = Some(tx);
        rx
    }
    pub fn set_pair_done(&self, rx: oneshot::Receiver<Result<Peer>>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pair_done = Some(rx);
    }
    pub fn take_pair_done(&self) -> Result<oneshot::Receiver<Result<Peer>>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pair_done
            .take()
            .ok_or_else(|| ClixError::Usage("not pairing".into()))
    }
    pub fn wake(&self) {
        self.woke.notify_waiters();
    }
}
impl MeshListener {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let handle = MeshHandle::default();
        handle.set_addr(listener.local_addr()?.to_string());
        Ok(Self { listener, handle })
    }
    pub fn handle(&self) -> MeshHandle {
        self.handle.clone()
    }
    pub fn local_addr(&self) -> String {
        self.handle.addr()
    }
    pub async fn run(&self, store: Arc<Mutex<Store>>) -> Result<()> {
        let config = transport::server_config(store.clone())?;
        loop {
            let permit = self.handle.limits.mesh().await;
            let (stream, _) = self.listener.accept().await?;
            let store = store.clone();
            let handle = self.handle.clone();
            let config = config.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(e) = serve_conn(store, handle, config, stream).await {
                    eprintln!("mesh connection rejected: {e}");
                }
            });
        }
    }
}

/// Keep owner control available while mesh plumbing is down; bind when it returns.
pub async fn run_available(store: Arc<Mutex<Store>>, handle: MeshHandle) {
    let mut previous_error = String::new();
    loop {
        let attempt = async {
            let mut mesh = MeshListener::bind(&listen_addr()?).await?;
            handle.set_addr(mesh.local_addr());
            mesh.handle = handle.clone();
            mesh.run(store.clone()).await
        }
        .await;
        handle.set_addr(String::new());
        if let Err(e) = attempt {
            if previous_error != e.to_string() {
                eprintln!("mesh unavailable: {e}; local owner commands remain available");
                previous_error = e.to_string();
            }
        }
        tokio::time::sleep(job::poll_interval()).await;
    }
}

fn configure_tcp(stream: &TcpStream) -> Result<()> {
    // Short framed RPCs must not wait for a delayed ACK between header and body.
    stream.set_nodelay(true).map_err(network_error)
}

pub async fn dial(addr: &str) -> Result<TcpStream> {
    let mut stream = tokio::time::timeout(IO_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| ClixError::Unreachable)?
        .map_err(network_error)?;
    configure_tcp(&stream)?;
    stream
        .write_all(PAIR_PREFACE)
        .await
        .map_err(network_error)?;
    Ok(stream)
}

pub(crate) fn network_error(e: std::io::Error) -> ClixError {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused | ConnectionReset | ConnectionAborted | BrokenPipe | TimedOut
        | NotConnected | UnexpectedEof | HostUnreachable | NetworkUnreachable => {
            ClixError::Unreachable
        }
        _ => ClixError::Protocol(format!("mesh transport failed: {e}")),
    }
}

pub async fn write_frame<S: AsyncWrite + Unpin>(stream: &mut S, data: &[u8]) -> Result<()> {
    if data.len() > MAX_FRAME {
        return Err(ClixError::Protocol("Clix message exceeds 64 MiB".into()));
    }
    stream
        .write_all(&(data.len() as u32).to_be_bytes())
        .await
        .map_err(network_error)?;
    stream.write_all(data).await.map_err(network_error)?;
    stream.flush().await.map_err(network_error)?;
    Ok(())
}
pub async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>> {
    let len = stream.read_u32().await.map_err(network_error)? as usize;
    if len > MAX_FRAME {
        return Err(ClixError::Protocol("Clix message exceeds 64 MiB".into()));
    }
    let mut data = vec![0; len];
    stream.read_exact(&mut data).await.map_err(network_error)?;
    Ok(data)
}

/// Both directions are authenticated to the paired keys. No plaintext fallback.
pub async fn call(addr: &str, sk: &[u8], expected_pk: &[u8], req: Value) -> Result<Value> {
    let exchange = async {
        let mut tcp = TcpStream::connect(addr).await.map_err(network_error)?;
        configure_tcp(&tcp)?;
        tcp.write_all(TLS_PREFACE).await.map_err(network_error)?;
        let mut tls = transport::connect(tcp, sk, expected_pk).await?;
        write_frame(&mut tls, &serde_json::to_vec(&req)?).await?;
        let response: Value = serde_json::from_slice(&read_frame(&mut tls).await?)?;
        if response.get("ok") == Some(&Value::Bool(false)) {
            if response["kind"] == "capacity" {
                return Err(ClixError::Capacity(
                    response["error"]
                        .as_str()
                        .unwrap_or("peer is at capacity")
                        .into(),
                ));
            }
            if response["kind"] == "unreachable" {
                return Err(ClixError::Unreachable);
            }
            if response["kind"] == "pin_conflict" {
                let field = |k: &str| {
                    response
                        .get(k)
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| ClixError::Protocol("invalid conflict response".into()))
                };
                return Err(ClixError::PinConflict {
                    path: field("path")?,
                    a: field("a")?,
                    b: field("b")?,
                });
            }
            return Err(ClixError::Protocol(
                response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("peer rejected request")
                    .into(),
            ));
        }
        Ok(response)
    };
    tokio::time::timeout(IO_TIMEOUT, exchange)
        .await
        .map_err(|_| ClixError::Unreachable)?
}

async fn serve_conn(
    store: Arc<Mutex<Store>>,
    handle: MeshHandle,
    config: Arc<rustls::ServerConfig>,
    mut stream: TcpStream,
) -> Result<()> {
    configure_tcp(&stream)?;
    let mut preface = [0; 5];
    tokio::time::timeout(IO_TIMEOUT, stream.read_exact(&mut preface))
        .await
        .map_err(|_| ClixError::Unreachable)?
        .map_err(network_error)?;
    if &preface == PAIR_PREFACE {
        let waiter = handle
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pair_tx
            .take();
        if let Some(tx) = waiter {
            let _ = tx.send(stream);
        }
        return Ok(());
    }
    if &preface != TLS_PREFACE {
        return Err(ClixError::Protocol("unsupported mesh protocol".into()));
    }
    let (mut tls, peer) = transport::accept(stream, config, &store).await?;
    let bytes = tokio::time::timeout(IO_TIMEOUT, read_frame(&mut tls))
        .await
        .map_err(|_| ClixError::Unreachable)??;
    let req: Value = serde_json::from_slice(&bytes)?;
    let response = match dispatch(&store, &handle.limits, &peer, req).await {
        Ok(v) => v,
        Err(ClixError::Capacity(reason)) => {
            json!({"ok":false,"kind":"capacity","error":reason})
        }
        Err(ClixError::Unreachable) => {
            json!({"ok":false,"kind":"unreachable","error":"machine is unreachable"})
        }
        Err(ClixError::PinConflict { path, a, b }) => {
            json!({"ok":false,"kind":"pin_conflict","path":path,"a":a,"b":b})
        }
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    };
    tokio::time::timeout(
        IO_TIMEOUT,
        write_frame(&mut tls, &serde_json::to_vec(&response)?),
    )
    .await
    .map_err(|_| ClixError::Unreachable)??;
    Ok(())
}

async fn dispatch(
    store: &Arc<Mutex<Store>>,
    limits: &crate::limits::Limits,
    peer: &Peer,
    req: Value,
) -> Result<Value> {
    match req.get("op").and_then(Value::as_str).unwrap_or("") {
        "exec_v2" | "job_get_v2" => {
            let c: crate::history::Certificate =
                serde_json::from_value(req.get("invocation").cloned().unwrap_or(Value::Null))?;
            {
                let s = store.lock().unwrap_or_else(|e| e.into_inner());
                s.ensure_writable()?;
                crate::history::authorize_execution(&s, peer, &c)?;
                if req["op"] == "job_get_v2" {
                    return crate::history::execution_reply(&s, &c);
                }
            }
            let existing = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .jobs
                .iter()
                .any(|j| j.id == c.invocation.id);
            let _exec_request = if existing {
                None
            } else {
                Some(limits.mesh_exec()?)
            };
            // Execution no longer triggers a pin sync (see dispatch_one).
            exec::submit_certified(store, limits, &c)?;
            crate::history::execution_reply(&store.lock().unwrap_or_else(|e| e.into_inner()), &c)
        }
        "history_page" => {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            let epoch = req.get("epoch").and_then(Value::as_str).unwrap_or("");
            if epoch.len() > 128 {
                return Err(ClixError::Protocol("invalid history epoch".into()));
            }
            let after = req
                .get("after")
                .and_then(Value::as_u64)
                .ok_or_else(|| ClixError::Protocol("missing history cursor".into()))?;
            Ok(serde_json::to_value(crate::history::page(
                &s,
                epoch,
                after,
                req.get("high").and_then(Value::as_u64),
            )?)?)
        }
        "exec" => {
            let argv: Vec<String> =
                serde_json::from_value(req.get("argv").cloned().unwrap_or(Value::Null))?;
            let id = req
                .get("job_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or(job::new_id()?);
            crate::limits::invocation(&id, &argv)?;
            let existing = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .jobs
                .iter()
                .any(|j| j.id == id);
            let _exec_request = if existing {
                None
            } else {
                Some(limits.mesh_exec()?)
            };
            // Execution no longer triggers a pin sync (see dispatch_one).
            exec::submit(store, limits, &peer.name, &argv, id)
        }
        "job_get" => {
            let id = req
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or_else(|| ClixError::Protocol("missing job ID".into()))?;
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            let j = s
                .jobs
                .iter()
                .find(|j| j.id == id && j.from == peer.name && j.body.0 == s.body_name);
            Ok(json!({"job":j}))
        }
        "request" => {
            let tool = req
                .get("tool")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ClixError::Usage("missing tool".into()))?;
            let r = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .update(|s| {
                    crate::request::receive(
                        s,
                        peer.name.clone(),
                        tool,
                        req.get("request_id").and_then(Value::as_str),
                    )
                })?;
            Ok(json!({"ok":true,"request":r}))
        }
        "pin_list" => crate::pin::rpc_list_async(store, peer).await,
        "pin_get" => crate::pin::rpc_get_async(store, &req).await,
        "pin_put" => crate::pin::rpc_put_async(store, peer, &req).await,
        "pin_commit" => crate::pin::rpc_commit_async(store, peer, &req).await,
        op => Err(ClixError::Usage(format!(
            "{op} is not allowed over the mesh"
        ))),
    }
}
