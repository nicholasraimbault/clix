use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::net::UnixListener;

use crate::cli::Cmd;
use crate::error::{ClixError, Result};
use crate::local::{self, client_send};
use crate::mesh::{self, MeshHandle, MeshListener};
use crate::paths::{socket_path, state_dir};
use crate::store::Store;

pub fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Daemon => run_daemon(),
        Cmd::Install => Err(ClixError::Usage("usage: clix install".into())),
        Cmd::Pair { phrase, name } => dispatch_pair(phrase, name),
        other => {
            let req = local::rpc_from_cmd(&other)?;
            let resp = client_send(&socket_path()?, req)?;
            local::emit_rpc(&other, &resp)
        }
    }
}

fn dispatch_pair(phrase: Option<String>, name: Option<String>) -> Result<()> {
    let sock = socket_path()?;
    match phrase {
        None => {
            let mut req = json!({"op": "pair_start"});
            if let Some(n) = name {
                req["name"] = json!(n);
            }
            let resp = client_send(&sock, req)?;
            let p = resp.get("phrase").and_then(Value::as_str).unwrap_or("");
            println!("pair with: {p}");
            client_send(&sock, json!({"op": "pair_await"}))?;
            Ok(())
        }
        Some(phrase) => {
            let mut req = json!({"op": "pair_join", "phrase": phrase});
            if let Some(n) = name {
                req["name"] = json!(n);
            }
            if let Ok(addr) = std::env::var("CLIX_PAIR_ADDR") {
                if !addr.is_empty() {
                    req["addr"] = json!(addr);
                }
            }
            client_send(&sock, req)?;
            Ok(())
        }
    }
}

fn run_daemon() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ClixError::Io(e.to_string()))?;
    rt.block_on(async {
        let store = Store::open(&state_dir()?)?;
        let bind = std::env::var("CLIX_MESH_BIND").unwrap_or_else(|_| "127.0.0.1:0".into());
        let mesh = MeshListener::bind(&bind).await?;
        serve(Arc::new(Mutex::new(store)), socket_path()?, mesh).await
    })
}

pub async fn serve_local(store: Arc<Mutex<Store>>, sock: PathBuf) -> Result<()> {
    let bind = std::env::var("CLIX_MESH_BIND").unwrap_or_else(|_| "127.0.0.1:0".into());
    let mesh = MeshListener::bind(&bind).await?;
    serve(store, sock, mesh).await
}

pub async fn serve(store: Arc<Mutex<Store>>, sock: PathBuf, mesh: MeshListener) -> Result<()> {
    let mesh_handle = mesh.handle();
    prepare_socket_path(&sock)?;
    let listener = UnixListener::bind(&sock)?;
    set_owner_mode(&sock)?;
    tokio::spawn(mesh::claim_waiting_jobs(
        store.clone(),
        mesh_handle.addr.clone(),
    ));
    tokio::select! {
        r = local_loop(store.clone(), mesh_handle, listener) => r,
        r = mesh.run(store.clone()) => r,
    }
}

async fn local_loop(
    store: Arc<Mutex<Store>>,
    mesh: MeshHandle,
    listener: UnixListener,
) -> Result<()> {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let store = store.clone();
                let mesh = mesh.clone();
                tokio::spawn(async move {
                    local::handle_connection(store, mesh, stream).await;
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

fn prepare_socket_path(sock: &Path) -> Result<()> {
    if let Some(parent) = sock.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    if sock.exists() {
        match UnixStream::connect(sock) {
            Ok(_) => {
                return Err(ClixError::Io(format!(
                    "already running at {}",
                    sock.display()
                )));
            }
            Err(_) => fs::remove_file(sock)?,
        }
    }
    Ok(())
}

fn set_owner_mode(sock: &Path) -> Result<()> {
    let mut perms = fs::metadata(sock)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(sock, perms)?;
    Ok(())
}
