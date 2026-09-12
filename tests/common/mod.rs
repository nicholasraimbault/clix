use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tokio::task::JoinHandle;

use clix::{client_send, serve, ClixError, MeshListener, Store};

static PAIR_ADDRS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn pair_addrs() -> &'static Mutex<HashMap<String, String>> {
    PAIR_ADDRS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct TestDaemon {
    _home: TempDir,
    pub sock: PathBuf,
    mesh_addr: String,
    handle: JoinHandle<Result<(), ClixError>>,
}

impl TestDaemon {
    #[allow(dead_code)]
    pub async fn spawn() -> Self {
        Self::spawn_named("test").await
    }

    pub async fn spawn_named(name: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = home.path().join("clix.sock");
        let mut store = Store::open(home.path()).unwrap();
        store.body_name = name.to_string();
        store.save().unwrap();
        let store = Arc::new(Mutex::new(store));
        let mesh = MeshListener::bind("127.0.0.1:0").await.unwrap();
        let mesh_addr = mesh.local_addr().to_string();
        let handle = tokio::spawn(serve(store, sock.clone(), mesh));
        wait_until_listening(&sock, &handle).await;
        Self {
            _home: home,
            sock,
            mesh_addr,
            handle,
        }
    }

    #[allow(dead_code)]
    pub fn mesh_addr(&self) -> &str {
        &self.mesh_addr
    }

    pub async fn rpc(&self, req: Value) -> Result<Value, ClixError> {
        let mut req = req;
        let op = req.get("op").and_then(Value::as_str).map(str::to_string);
        if op.as_deref() == Some("pair_join") && req.get("addr").and_then(Value::as_str).is_none() {
            if let Some(phrase) = req.get("phrase").and_then(Value::as_str) {
                if let Some(addr) = pair_addrs()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(phrase)
                    .cloned()
                {
                    req["addr"] = Value::String(addr);
                }
            }
        }
        let sock = self.sock.clone();
        let result = tokio::task::spawn_blocking(move || client_send(&sock, req))
            .await
            .expect("client_send task");
        if op.as_deref() == Some("pair_start") {
            if let Ok(v) = result.as_ref() {
                if let Some(phrase) = v.get("phrase").and_then(Value::as_str) {
                    pair_addrs()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(phrase.to_string(), self.mesh_addr.clone());
                }
            }
        }
        result
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn wait_until_listening(sock: &std::path::Path, handle: &JoinHandle<Result<(), ClixError>>) {
    let start = Instant::now();
    loop {
        if sock.exists() && std::os::unix::net::UnixStream::connect(sock).is_ok() {
            return;
        }
        if handle.is_finished() {
            panic!("daemon exited before listening on {}", sock.display());
        }
        if start.elapsed() > Duration::from_secs(2) {
            panic!("timed out waiting for {}", sock.display());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
