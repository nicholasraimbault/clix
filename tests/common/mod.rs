use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tokio::task::JoinHandle;

use clix::{client_send, serve_local, ClixError, Store};

pub struct TestDaemon {
    _home: TempDir,
    pub sock: PathBuf,
    handle: JoinHandle<Result<(), ClixError>>,
}

impl TestDaemon {
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
        let handle = tokio::spawn(serve_local(store, sock.clone()));
        wait_until_listening(&sock, &handle).await;
        Self {
            _home: home,
            sock,
            handle,
        }
    }

    pub async fn rpc(&self, req: Value) -> Result<Value, ClixError> {
        let sock = self.sock.clone();
        tokio::task::spawn_blocking(move || client_send(&sock, req))
            .await
            .expect("client_send task")
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
