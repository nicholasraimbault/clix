use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ClixError, Result};
use crate::types::{Grant, Job, JobStatus, OutboundRequest, Peer, Request, RequestReceipt};

/// Last successful pin sync: content hashes plus when that sync finished.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinIndex {
    #[serde(default)]
    pub peers: BTreeMap<String, BTreeMap<String, crate::pin::Fingerprint>>,
    /// Unix milliseconds of last successful pin sync.
    #[serde(default)]
    pub last_sync: Option<u64>,
    /// Relative path → sha256 hex at last successful sync.
    #[serde(default)]
    pub hashes: BTreeMap<String, String>,
    /// Set on pin conflict; cleared only after a successful sync.
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(skip)]
    dir: PathBuf,
    #[serde(skip)]
    write_error: Option<String>,
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
    #[serde(default)]
    pub outbound_requests: Vec<OutboundRequest>,
    #[serde(default)]
    pub request_receipts: Vec<RequestReceipt>,
    #[serde(default)]
    pub pin_index: PinIndex,
    #[serde(default)]
    pub history: crate::history::History,
    #[serde(default)]
    pub retry_links: BTreeMap<String, String>,
    #[serde(default)]
    pub job_certificates: BTreeMap<String, String>,
    #[serde(default)]
    pub legacy_jobs: BTreeSet<String>,
    #[serde(default)]
    pub output_pruned: BTreeSet<String>,
    #[serde(default)]
    pub history_pruned: BTreeSet<String>,
    #[serde(default)]
    pub format_version: u32,
    /// Test/override pin tree. Production uses `CLIX_PIN` or `~/src`.
    #[serde(skip)]
    pub pin_dir: Option<PathBuf>,
}

impl Store {
    pub fn lock_dir(dir: &Path) -> Result<fs::File> {
        private_dir(dir)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(dir.join("daemon.lock"))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
            |_| ClixError::Usage("another Clix sidecar is using this state directory".into()),
        )?;
        Ok(file)
    }

    pub fn open(dir: &Path) -> Result<Store> {
        private_dir(dir)?;
        let path = dir.join("state.json");
        if path.exists() {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&path)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            let mut bytes = Vec::new();
            file.take((crate::storage::STATE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > crate::storage::STATE_BYTES {
                return Err(ClixError::Capacity(
                    "saved Clix state exceeds the supported 128 MiB limit; file preserved".into(),
                ));
            }
            let mut store: Store = serde_json::from_slice(&bytes)?;
            if store.format_version > 1 {
                return Err(ClixError::Usage(
                    "this state needs a newer Clix binary".into(),
                ));
            }
            store.dir = dir.to_path_buf();
            if store.body_name.is_empty()
                && store.peers.is_empty()
                && store.jobs.is_empty()
                && store.grants.is_empty()
            {
                store.body_name = crate::pair::hostname();
                store.save()?;
            }
            if store.owner_sk.is_empty() {
                store.owner_sk = generate_sk()?;
                store.save()?;
            }
            Ok(store)
        } else {
            let mut store = Store {
                dir: dir.to_path_buf(),
                write_error: None,
                body_name: crate::pair::hostname(),
                owner_sk: generate_sk()?,
                peers: Vec::new(),
                grants: Vec::new(),
                jobs: Vec::new(),
                requests: Vec::new(),
                outbound_requests: Vec::new(),
                request_receipts: Vec::new(),
                pin_index: PinIndex::default(),
                history: crate::history::History::default(),
                retry_links: BTreeMap::new(),
                job_certificates: BTreeMap::new(),
                legacy_jobs: BTreeSet::new(),
                output_pruned: BTreeSet::new(),
                history_pruned: BTreeSet::new(),
                format_version: 1,
                pin_dir: None,
            };
            crate::history::refresh_owned(&mut store, None)?;
            store.save()?;
            Ok(store)
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_encoded(&crate::storage::encode(self)?)
    }

    fn save_encoded(&self, json: &[u8]) -> Result<()> {
        private_dir(&self.dir)?;
        let path = self.dir.join("state.json");
        let tmp = self
            .dir
            .join(format!("state-{}.tmp", crate::job::new_id()?));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(json)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            fs::File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }

    pub fn ensure_writable(&self) -> Result<()> {
        if let Some(reason) = &self.write_error {
            return Err(ClixError::Io(format!(
                "Clix storage is unavailable: {reason}. Repair storage and restart the sidecar."
            )));
        }
        Ok(())
    }

    pub(crate) fn storage_error(&self) -> Option<&str> {
        self.write_error.as_deref()
    }

    /// Stop mutations after a recovery failure, even when it was a policy
    /// limit rather than an I/O error. Last durable state is still inspectable.
    pub(crate) fn recovery_failed(&mut self, error: &ClixError) {
        self.write_error.get_or_insert_with(|| error.to_string());
        for job in &mut self.jobs {
            if job.body.0 == self.body_name && matches!(job.status, JobStatus::Running) {
                job.status = JobStatus::Uncertain {
                    reason: "sidecar restarted during execution and could not save recovery; the command may have taken effect. Not rerunning.".into(),
                };
            }
        }
    }

    /// Publish state only after its durable replacement succeeds.
    pub fn update<T>(&mut self, edit: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
        self.ensure_writable()?;
        let mut next = self.clone();
        let value = edit(&mut next)?;
        crate::history::refresh_owned(&mut next, Some(self))?;
        next.format_version = 1;
        crate::history::normalize_output(&mut next);
        crate::storage::prepare(&mut next, self)?;
        let bytes = crate::storage::encode(&next)?;
        crate::storage::reserve_completion(&next, self, bytes.len())?;
        if let Err(e) = next.save_encoded(&bytes) {
            self.write_error = Some(e.to_string());
            return Err(e);
        }
        *self = next;
        Ok(value)
    }

    pub fn recover_jobs(&mut self) -> Result<()> {
        if self.history.initialized
            && !self
                .jobs
                .iter()
                .any(|j| j.body.0 == self.body_name && matches!(j.status, JobStatus::Running))
        {
            return Ok(());
        }
        self.update(|s| {
            for j in &mut s.jobs {
                if j.body.0 == s.body_name && matches!(j.status, JobStatus::Running) {
                    j.status = JobStatus::Uncertain { reason: "sidecar restarted during execution; the command may have taken effect. Not rerunning.".into() };
                }
            }
            Ok(())
        })
    }

    pub fn append_job(&mut self, job: Job) -> Result<()> {
        self.put_job(job)
    }

    pub fn put_job(&mut self, job: Job) -> Result<()> {
        self.update(|s| {
            if let Some(existing) = s.jobs.iter_mut().find(|j| j.id == job.id) {
                *existing = job;
            } else {
                s.jobs.push(job);
            }
            Ok(())
        })
    }
}

fn private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    if fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(ClixError::Usage(
            "Clix state directory must not be a symlink".into(),
        ));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
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
