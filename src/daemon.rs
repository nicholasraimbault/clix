use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::net::UnixListener;

use crate::cli::Cmd;
use crate::error::{ClixError, Result};
use crate::local::{self, client_send};
use crate::paths::{socket_path, state_dir};
use crate::store::Store;

pub fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Daemon => run_daemon(),
        Cmd::Install => Err(ClixError::Usage("usage: clix install".into())),
        other => {
            let req = local::rpc_from_cmd(&other)?;
            let resp = client_send(&socket_path()?, req)?;
            local::emit_rpc(&other, &resp)
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
        serve_local(Arc::new(Mutex::new(store)), socket_path()?).await
    })
}

pub async fn serve_local(store: Arc<Mutex<Store>>, sock: PathBuf) -> Result<()> {
    prepare_socket_path(&sock)?;
    let listener = UnixListener::bind(&sock)?;
    set_owner_mode(&sock)?;
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let store = store.clone();
                tokio::spawn(async move {
                    local::handle_connection(store, stream).await;
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
