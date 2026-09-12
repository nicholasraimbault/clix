use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};

use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::{BodyId, Grant, Schedule};

/// Resolve `name` to an executable: absolute (or slash-containing) path, else PATH (`which`).
pub fn resolve_tool(name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() || name.contains('/') {
        return existing_executable(path, name);
    }
    let path_var = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Ok(std::path::absolute(candidate)?);
        }
    }
    Err(ClixError::NoSuchTool {
        tool: name.to_string(),
    })
}

fn existing_executable(path: &Path, name: &str) -> Result<PathBuf> {
    if is_executable_file(path) {
        Ok(std::path::absolute(path)?)
    } else {
        Err(ClixError::NoSuchTool {
            tool: name.to_string(),
        })
    }
}

fn is_executable_file(path: &Path) -> bool {
    match path.metadata() {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn tool_key(name: &str) -> String {
    Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name)
        .to_string()
}

/// Add a grant on this box. Empty `allow` means every body (`allow_from: None`).
/// `--server` is CLI-only; here it is just a name in `allow` / `allow_from`.
pub fn add(
    store: &mut Store,
    tool: &str,
    allow: &[String],
    once: bool,
    until: Option<SystemTime>,
    schedule: Option<Schedule>,
) -> Result<Grant> {
    if once && schedule.is_some() {
        return Err(ClixError::Usage(
            "use --once or a schedule, not both".into(),
        ));
    }
    let binary = resolve_tool(tool)?;
    for name in allow {
        if *name != store.body_name && !store.peers.iter().any(|p| p.name.0 == *name) {
            return Err(ClixError::Usage(format!("{name} is not a paired body")));
        }
    }
    let allow_from = if allow.is_empty() {
        None
    } else {
        Some(allow.iter().cloned().map(BodyId).collect())
    };
    let grant = Grant {
        tool: tool_key(tool),
        binary,
        allow_from,
        once,
        until,
        schedule,
        reservation: None,
    };
    store.grants.retain(|g| g.tool != grant.tool);
    store.grants.push(grant.clone());
    Ok(grant)
}

pub fn remove(store: &mut Store, tool: &str) -> Result<()> {
    let before = store.grants.len();
    store.grants.retain(|g| g.tool != tool);
    if store.grants.len() == before {
        Err(ClixError::NotAdded {
            tool: tool.to_string(),
            body: store.body_name.clone(),
        })
    } else {
        Ok(())
    }
}

pub fn hands(store: &Store) -> &[Grant] {
    &store.grants
}

pub fn check(store: &Store, tool: &str, from: &BodyId) -> Result<Grant> {
    check_at(store, tool, from, Local::now())
}

/// Same as `check`, with an injectable local clock for schedules and `until`.
pub fn check_at(store: &Store, tool: &str, from: &BodyId, now: DateTime<Local>) -> Result<Grant> {
    let grant = store
        .grants
        .iter()
        .find(|g| g.tool == tool)
        .cloned()
        .ok_or_else(|| ClixError::NotAdded {
            tool: tool.to_string(),
            body: store.body_name.clone(),
        })?;

    if grant.reservation.is_some() {
        return Err(ClixError::GrantBusy { tool: grant.tool });
    }

    if let Some(ref allowed) = grant.allow_from {
        if !allowed.iter().any(|b| b == from) {
            return Err(ClixError::NotAllowed {
                tool: grant.tool,
                from: from.to_string(),
                body: store.body_name.clone(),
            });
        }
    }

    let now_st = SystemTime::from(now);
    if let Some(until) = grant.until {
        if until < now_st {
            return Err(ClixError::Expired {
                tool: grant.tool,
                body: store.body_name.clone(),
            });
        }
    }

    if let Some(ref schedule) = grant.schedule {
        if !schedule.allows_at(now) {
            return Err(ClixError::NotAddedAtTime {
                tool: grant.tool,
                body: store.body_name.clone(),
            });
        }
    }

    Ok(grant)
}
