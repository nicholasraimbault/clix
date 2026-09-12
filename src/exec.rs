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
    exec_with_job(store, from, argv, None)
}

/// Same as `exec_checked`, reusing `job_id` so a wait retry does not run twice.
pub(crate) fn exec_with_job(
    store: &Arc<Mutex<Store>>,
    from: &BodyId,
    argv: &[String],
    job_id: Option<String>,
) -> Result<Value> {
    if argv.is_empty() {
        return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
    }
    let id = match job_id.clone() {
        Some(id) => id,
        None => job::new_id()?,
    };
    let (grant, body) = {
        let mut s = lock(store);
        let body = BodyId(s.body_name.clone());
        if let Some(ref jid) = job_id {
            if let Some(job) = s.jobs.iter().find(|j| j.id == *jid) {
                if job::is_terminal(&job.status) || matches!(job.status, JobStatus::Running) {
                    return Ok(job::exec_json(job));
                }
            }
        }
        match grant::check(&s, &argv[0], from) {
            Ok(g) => {
                if job_id.is_some() {
                    if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                        j.status = JobStatus::Running;
                    } else {
                        s.jobs.push(Job {
                            id: id.clone(),
                            body: body.clone(),
                            argv: argv.to_vec(),
                            from: from.clone(),
                            status: JobStatus::Running,
                        });
                    }
                    s.save()?;
                }
                (g, body)
            }
            Err(e) => {
                let reason = e.to_string();
                let job = Job {
                    id,
                    body,
                    argv: argv.to_vec(),
                    from: from.clone(),
                    status: JobStatus::Denied {
                        reason: reason.clone(),
                    },
                };
                s.put_job(job.clone())?;
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
            let job = finish_job(store, id, from.clone(), body, argv.to_vec(), job_status)?;
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
                    id,
                    body,
                    argv: argv.to_vec(),
                    from: from.clone(),
                    status: job_status,
                };
                s.put_job(job.clone())?;
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

fn finish_job(
    store: &Arc<Mutex<Store>>,
    id: String,
    from: BodyId,
    body: BodyId,
    argv: Vec<String>,
    status: JobStatus,
) -> Result<Job> {
    let job = Job {
        id,
        body,
        argv,
        from,
        status,
    };
    job::put(store, job)
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
