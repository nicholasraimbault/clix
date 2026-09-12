use std::time::SystemTime;

use crate::error::{ClixError, Result};
use crate::grant;
use crate::job;
use crate::store::Store;
use crate::types::{BodyId, Grant, Request, Schedule};

pub(crate) fn receive(
    store: &mut Store,
    from: BodyId,
    tool: &str,
    delivery_id: Option<&str>,
) -> Result<Request> {
    if let Some(id) = delivery_id {
        if id.is_empty() || id.len() > 128 {
            return Err(ClixError::Protocol("invalid request ID".into()));
        }
        if let Some(receipt) = store
            .request_receipts
            .iter()
            .find(|r| r.delivery_id == id && r.request.from == from)
        {
            if receipt.request.tool != tool {
                return Err(ClixError::Protocol(
                    "request ID belongs to a different tool".into(),
                ));
            }
            return Ok(receipt.request.clone());
        }
    }
    let request = upsert(store, from, tool)?;
    if let Some(id) = delivery_id {
        store.request_receipts.push(crate::types::RequestReceipt {
            delivery_id: id.into(),
            request: request.clone(),
        });
    }
    Ok(request)
}

pub(crate) async fn deliver(
    store: &std::sync::Arc<std::sync::Mutex<Store>>,
    request: &crate::types::OutboundRequest,
) -> Result<()> {
    let (peer, sk) = {
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        (
            s.peers.iter().find(|p| p.name == request.body).cloned(),
            s.owner_sk.clone(),
        )
    };
    let result = async {
        let peer =
            peer.ok_or_else(|| ClixError::Usage(format!("{} is not paired", request.body)))?;
        crate::mesh::call(
            peer.addr.as_deref().ok_or(ClixError::Unreachable)?,
            &sk,
            &peer.owner_pk,
            serde_json::json!({"op":"request", "tool":request.tool, "request_id":request.id}),
        )
        .await
    }
    .await;
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    s.ensure_writable()?;
    let Some(existing) = s.outbound_requests.iter().find(|r| r.id == request.id) else {
        return Ok(());
    };
    if existing.waiting && matches!(result, Err(ClixError::Unreachable)) {
        return Ok(());
    }
    s.update(|s| {
        if let Some(index) = s.outbound_requests.iter().position(|r| r.id == request.id) {
            match result {
                Ok(_) => {
                    s.outbound_requests.remove(index);
                }
                Err(ClixError::Unreachable) => {
                    s.outbound_requests[index].waiting = true;
                }
                Err(e) => {
                    s.outbound_requests[index].error = Some(e.to_string());
                }
            }
        }
        Ok(())
    })
}

/// Record a grant request on this body. Same from+tool refreshes the existing row.
pub fn upsert(store: &mut Store, from: BodyId, tool: &str) -> Result<Request> {
    if tool.is_empty() {
        return Err(ClixError::Usage("usage: clix request <body> <tool>".into()));
    }
    let req = if let Some(existing) = store
        .requests
        .iter_mut()
        .rev()
        .find(|r| r.from == from && r.tool == tool)
    {
        existing.once_suggested = true;
        existing.clone()
    } else {
        let req = Request {
            id: job::new_id()?,
            from,
            tool: tool.to_string(),
            once_suggested: true,
        };
        store.requests.push(req.clone());
        req
    };
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

/// Apply a notification action to a specific pending request.
///
/// `once` grants `--once` for the requester. `allow` grants until remove for
/// every body. `deny` drops that row.
pub fn apply_action(store: &mut Store, id: &str, action: &str) -> Result<()> {
    let idx = store
        .requests
        .iter()
        .position(|r| r.id == id)
        .ok_or_else(|| ClixError::Usage("no pending request".into()))?;
    let req = store.requests[idx].clone();
    match action {
        "once" => {
            grant::add(
                store,
                &req.tool,
                std::slice::from_ref(&req.from.0),
                true,
                None,
                None,
            )?;
            store.requests.remove(idx);
        }
        "allow" => {
            grant::add(store, &req.tool, &[], false, None, None)?;
            store.requests.remove(idx);
        }
        "deny" => {
            store.requests.remove(idx);
        }
        _ => return Ok(()),
    }
    Ok(())
}
