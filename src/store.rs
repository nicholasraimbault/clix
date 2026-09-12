use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ClixError, Result};
use crate::types::{Grant, Job, Peer, Request};

#[derive(Debug, Serialize, Deserialize)]
pub struct Store {
    #[serde(skip)]
    dir: PathBuf,
    pub body_name: String,
    pub owner_sk: Vec<u8>,
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(default)]
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub jobs: Vec<Job>,
    #[serde(default)]
    pub requests: Vec<Request>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store> {
        fs::create_dir_all(dir)?;
        let path = dir.join("state.json");
        if path.exists() {
            let bytes = fs::read(&path)?;
            let mut store: Store = serde_json::from_slice(&bytes)?;
            store.dir = dir.to_path_buf();
            if store.owner_sk.is_empty() {
                store.owner_sk = generate_sk()?;
                store.save()?;
            }
            Ok(store)
        } else {
            let store = Store {
                dir: dir.to_path_buf(),
                body_name: String::new(),
                owner_sk: generate_sk()?,
                peers: Vec::new(),
                grants: Vec::new(),
                jobs: Vec::new(),
                requests: Vec::new(),
            };
            store.save()?;
            Ok(store)
        }
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join("state.json");
        let tmp = self.dir.join("state.json.tmp");
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn append_job(&mut self, job: Job) -> Result<()> {
        self.put_job(job)
    }

    pub fn put_job(&mut self, job: Job) -> Result<()> {
        if let Some(existing) = self.jobs.iter_mut().find(|j| j.id == job.id) {
            *existing = job;
        } else {
            self.jobs.push(job);
        }
        self.save()
    }
}

fn generate_sk() -> Result<Vec<u8>> {
    // 32-byte ed25519 seed
    let mut buf = vec![0u8; 32];
    let mut f = fs::File::open("/dev/urandom")
        .map_err(|e| ClixError::Io(format!("read /dev/urandom: {e}")))?;
    f.read_exact(&mut buf)
        .map_err(|e| ClixError::Io(format!("read /dev/urandom: {e}")))?;
    Ok(buf)
}
