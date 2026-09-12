use std::time::SystemTime;

use crate::error::{ClixError, Result};
use crate::grant;
use crate::job;
use crate::store::Store;
use crate::types::{BodyId, Grant, Request, Schedule};

/// Record a grant request on this body. Same from+tool refreshes the existing row.
pub fn upsert(store: &mut Store, from: BodyId, tool: &str) -> Result<Request> {
    if tool.is_empty() {
        return Err(ClixError::Usage("usage: clix request <body> <tool>".into()));
    }
    if let Some(existing) = store
        .requests
        .iter_mut()
        .rev()
        .find(|r| r.from == from && r.tool == tool)
    {
        existing.once_suggested = true;
        return Ok(existing.clone());
    }
    let req = Request {
        id: job::new_id()?,
        from,
        tool: tool.to_string(),
        once_suggested: true,
    };
    store.requests.push(req.clone());
    Ok(req)
}

pub fn pending(store: &Store) -> &[Request] {
    &store.requests
}

/// Grant the latest request. Default is `--once` for the requesting body.
pub fn allow(
    store: &mut Store,
    allow: &[String],
    once: bool,
    until: Option<SystemTime>,
    schedule: Option<Schedule>,
) -> Result<Grant> {
    let req = store
        .requests
        .last()
        .cloned()
        .ok_or_else(|| ClixError::Usage("no pending request".into()))?;
    let allow = if allow.is_empty() {
        vec![req.from.0.clone()]
    } else {
        allow.to_vec()
    };
    let grant = grant::add(store, &req.tool, &allow, once, until, schedule)?;
    store.requests.pop();
    Ok(grant)
}

pub fn deny(store: &mut Store) -> Result<Request> {
    store
        .requests
        .pop()
        .ok_or_else(|| ClixError::Usage("no pending request".into()))
}
