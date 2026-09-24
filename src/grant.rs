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
    let grant = build(store, tool, allow, once, until, schedule)?;
    commit(store, grant.clone());
    Ok(grant)
}

/// Construct the grant an `add`/approval would produce, validating the tool and
/// allow-list, without touching `store`. Callers that must not silently replace
/// an existing grant compare this against the current one before committing.
pub(crate) fn build(
    store: &Store,
    tool: &str,
    allow: &[String],
    once: bool,
    until: Option<SystemTime>,
    schedule: Option<Schedule>,
) -> Result<Grant> {
    crate::limits::tool(tool)?;
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
    Ok(Grant {
        tool: tool_key(tool),
        binary,
        allow_from,
        once,
        until,
        schedule,
        reservation: None,
    })
}

/// Replace any grant for the same tool with this one and invalidate its pending
/// requests. This is the explicit owner path (`clix add`); the request-approval
/// path guards against replacing a *different* grant before calling it.
pub(crate) fn commit(store: &mut Store, grant: Grant) {
    store.grants.retain(|g| g.tool != grant.tool);
    store.grants.push(grant.clone());
    invalidate_requests(store, &grant.tool);
}

/// Two grants describe the same permission, ignoring a transient run reservation.
pub(crate) fn same_permission(a: &Grant, b: &Grant) -> bool {
    a.tool == b.tool
        && a.binary == b.binary
        && a.allow_from == b.allow_from
        && a.once == b.once
        && a.until == b.until
        && a.schedule == b.schedule
}

/// Resolve only the spelling of a removal path, never its filesystem target.
/// The CLI calls this before RPC so relative paths use the owner's working directory.
pub(crate) fn removal_target(tool: &str) -> Result<PathBuf> {
    if tool.contains('/') {
        Ok(std::path::absolute(tool)?)
    } else {
        Ok(PathBuf::from(tool))
    }
}

fn invalidate_requests(store: &mut Store, key: &str) {
    // A pending approval cannot supersede a later owner decision for this slot.
    // Delivery receipts remain, so delayed retries cannot revive its old ID.
    store.requests.retain(|r| tool_key(&r.tool) != key);
}

pub fn remove(store: &mut Store, tool: &str) -> Result<()> {
    let target = removal_target(tool)?;
    let index = store.grants.iter().position(|g| {
        if target.is_absolute() {
            g.binary == target
        } else {
            g.tool == tool
        }
    });
    let index = index.ok_or_else(|| ClixError::NotAdded {
        tool: tool.to_string(),
        body: store.body_name.clone(),
    })?;
    let removed = store.grants.remove(index);
    invalidate_requests(store, &removed.tool);
    Ok(())
}

pub fn hands(store: &Store) -> &[Grant] {
    &store.grants
}

/// One human-readable line for a grant, showing what `clix hands` used to hide:
/// which machines it applies to, whether it is single-use, its expiry and any
/// schedule.
pub fn describe(grant: &Grant) -> String {
    let scope = match &grant.allow_from {
        None => "all machines".to_string(),
        Some(bodies) if bodies.is_empty() => "no machines".to_string(),
        Some(bodies) => bodies
            .iter()
            .map(|b| b.0.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    };
    let mut line = format!("{}  → {scope}", grant.tool);
    if grant.once {
        line.push_str("  once");
    }
    if let Some(until) = grant.until {
        let secs = until
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let when = DateTime::<Local>::from(until).format("%Y-%m-%d %H:%M");
        line.push_str(&format!("  until {when} ({secs})"));
    }
    if grant.schedule.is_some() {
        line.push_str("  schedule");
    }
    if grant.reservation.is_some() {
        line.push_str("  [reserved for a running job]");
    }
    line
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
