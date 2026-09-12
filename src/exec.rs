use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{ClixError, Result};
use crate::grant::{self, consume_once};
use crate::job;
use crate::store::Store;
use crate::types::{BodyId, Grant, Job, JobStatus};

/// Run the granted binary. `argv[0]` must be `grant.tool`. Not a shell.
pub fn run_granted(grant: &Grant, argv: &[String]) -> Result<Output> {
    let argv0 = argv.first().map(String::as_str).unwrap_or("");
    if argv0 != grant.tool {
        return Err(ClixError::Usage(format!("{argv0} is not granted")));
    }
    Ok(Command::new(&grant.binary).args(&argv[1..]).output()?)
}

/// `check` then `run_granted`. `from` is the calling body (local name or authenticated peer).
pub(crate) fn exec_checked(
    store: &Arc<Mutex<Store>>,
    from: &BodyId,
    argv: &[String],
) -> Result<Value> {
    if argv.is_empty() {
        return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
    }
    let (grant, body) = {
        let s = lock(store);
        let body = BodyId(s.body_name.clone());
        match grant::check(&s, &argv[0], from) {
            Ok(g) => (g, body),
            Err(e) => {
                drop(s);
                let reason = e.to_string();
                let job = job::append(
                    store,
                    from.clone(),
                    body,
                    argv.to_vec(),
                    JobStatus::Denied {
                        reason: reason.clone(),
                    },
                )?;
                return Ok(denied_json(reason, job));
            }
        }
    };
    match run_granted(&grant, argv) {
        Err(e) => {
            let reason = e.to_string();
            let (status, job_status) = match e {
                ClixError::Io(_) => (
                    "failed",
                    JobStatus::Failed {
                        reason: reason.clone(),
                    },
                ),
                _ => (
                    "denied",
                    JobStatus::Denied {
                        reason: reason.clone(),
                    },
                ),
            };
            let job = job::append(store, from.clone(), body, argv.to_vec(), job_status)?;
            Ok(json!({
                "status": status,
                "reason": reason,
                "job": job,
            }))
        }
        Ok(out) => {
            let exit = out.status.code().unwrap_or(1);
            let job_status = JobStatus::Done { exit };
            let job = {
                let mut s = lock(store);
                if out.status.success() {
                    consume_once(&mut s, &grant.tool);
                }
                let job = Job {
                    id: job::new_id()?,
                    body,
                    argv: argv.to_vec(),
                    from: from.clone(),
                    status: job_status,
                };
                s.append_job(job.clone())?;
                job
            };
            Ok(json!({
                "status": "done",
                "exit": exit,
                "stdout": String::from_utf8_lossy(&out.stdout),
                "stderr": String::from_utf8_lossy(&out.stderr),
                "job": job,
            }))
        }
    }
}

fn denied_json(reason: String, job: Job) -> Value {
    json!({
        "status": "denied",
        "reason": reason,
        "job": job,
    })
}

fn lock(store: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}
