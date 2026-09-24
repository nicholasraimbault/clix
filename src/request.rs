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
    crate::limits::tool(tool)?;
    if let Some(id) = delivery_id {
        crate::limits::identifier(id)?;
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
        s.ensure_writable()?;
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
    crate::limits::tool(tool)?;
    if tool.is_empty() {
        return Err(ClixError::Usage(
            "usage: clix request <machine> <tool>".into(),
        ));
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

#[derive(Clone)]
pub(crate) enum Scope {
    Requester,
    Bodies(Vec<String>),
}

#[derive(Clone)]
pub(crate) enum Decision {
    Allow {
        scope: Scope,
        once: bool,
        until: Option<SystemTime>,
        schedule: Option<Schedule>,
    },
    Deny,
}

impl Decision {
    /// Native action names are translated once, with their authority explicit.
    pub(crate) fn from_action(action: &str) -> Result<Self> {
        match action {
            "default" | "once" => Ok(Self::Allow {
                scope: Scope::Requester,
                once: true,
                until: None,
                schedule: None,
            }),
            // "Allow" persists but is scoped to the requesting machine, not all
            // paired machines. Broad grants are explicit: clix add --all.
            "allow" => Ok(Self::Allow {
                scope: Scope::Requester,
                once: false,
                until: None,
                schedule: None,
            }),
            "deny" => Ok(Self::Deny),
            _ => Err(ClixError::Usage("unknown request action".into())),
        }
    }
}

/// Caller commits this decision in Store::update. IDs never select another row.
pub(crate) fn decide(store: &mut Store, id: &str, decision: Decision) -> Result<Option<Grant>> {
    let idx = store
        .requests
        .iter()
        .position(|r| r.id == id)
        .filter(|_| !id.is_empty())
        .ok_or_else(|| ClixError::Usage("request is no longer pending; run clix pending".into()))?;
    let req = store.requests[idx].clone();
    match decision {
        Decision::Allow {
            scope,
            once,
            until,
            schedule,
        } => {
            let allow = match scope {
                Scope::Requester => vec![req.from.0],
                Scope::Bodies(bodies) if !bodies.is_empty() => bodies,
                Scope::Bodies(_) => {
                    return Err(ClixError::Usage(
                        "approval body list must not be empty".into(),
                    ))
                }
            };
            let prospective = grant::build(store, &req.tool, &allow, once, until, schedule, None)?;
            // Approving a request must not silently replace a *different*
            // existing grant for the same tool (dropping its allow-list, expiry
            // or schedule, or revoking another machine). The owner changes an
            // existing grant explicitly with clix add / clix remove. An
            // identical re-approval is idempotent and still clears the request.
            if let Some(existing) = store.grants.iter().find(|g| g.tool == prospective.tool) {
                if !grant::same_permission(existing, &prospective) {
                    return Err(ClixError::Usage(format!(
                        "{} already has a grant on {}; change it with clix add / clix remove rather than approving this request",
                        prospective.tool, store.body_name
                    )));
                }
            }
            // commit also invalidates every older request for this grant slot.
            grant::commit(store, prospective.clone());
            Ok(Some(prospective))
        }
        Decision::Deny => {
            store.requests.remove(idx);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Peer;

    #[test]
    fn native_allow_grants_only_the_requesting_machine_persistently() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.body_name = "laptop".into();
        for name in ["server", "phone"] {
            store.peers.push(Peer {
                name: BodyId(name.into()),
                owner_pk: vec![1; 32],
                addr: None,
            });
        }
        let req = store
            .update(|s| upsert(s, BodyId("server".into()), "true"))
            .unwrap();
        store
            .update(|s| decide(s, &req.id, Decision::from_action("allow")?))
            .unwrap();
        // "Allow" persists (not once) but is scoped to the requester, not all
        // paired machines: phone is not granted.
        assert!(!store.grants[0].once);
        assert_eq!(
            store.grants[0].allow_from,
            Some(vec![BodyId("server".into())])
        );
    }

    #[test]
    fn approval_never_silently_replaces_a_differing_grant() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.body_name = "laptop".into();
        for name in ["server", "phone"] {
            store.peers.push(Peer {
                name: BodyId(name.into()),
                owner_pk: vec![1; 32],
                addr: None,
            });
        }
        // Owner has already granted `true` to the server only.
        store
            .update(|s| crate::grant::add(s, "true", &["server".into()], false, None, None))
            .unwrap();

        // A request from phone for the same tool must NOT be approvable in a way
        // that drops the server-only scope: it is refused, grant untouched.
        let req = store
            .update(|s| upsert(s, BodyId("phone".into()), "true"))
            .unwrap();
        let before = store.grants.clone();
        let err = store
            .update(|s| decide(s, &req.id, Decision::from_action("once")?))
            .unwrap_err();
        assert!(err.to_string().contains("clix add"), "{err}");
        assert_eq!(store.grants, before);
        // The request stays pending for the owner to handle explicitly.
        assert!(store.requests.iter().any(|r| r.id == req.id));

        // Approving a request for a tool with no existing grant still works.
        let fresh = store
            .update(|s| upsert(s, BodyId("phone".into()), "false"))
            .unwrap();
        store
            .update(|s| decide(s, &fresh.id, Decision::from_action("once")?))
            .unwrap();
        assert_eq!(store.grants.len(), 2);
    }

    #[test]
    fn native_decisions_keep_their_scope_and_failed_persistence_keeps_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.body_name = "laptop".into();
        store.peers.push(Peer {
            name: BodyId("server".into()),
            owner_pk: vec![1; 32],
            addr: None,
        });
        let req = store
            .update(|s| upsert(s, BodyId("server".into()), "true"))
            .unwrap();
        store
            .update(|s| decide(s, &req.id, Decision::from_action("default")?))
            .unwrap();
        assert!(store.grants[0].once);
        assert_eq!(
            store.grants[0].allow_from,
            Some(vec![BodyId("server".into())])
        );
        // A second approval for the same tool that would change the grant is
        // refused rather than silently replacing it; the first grant stands.
        let req = store
            .update(|s| upsert(s, BodyId("server".into()), "true"))
            .unwrap();
        assert!(store
            .update(|s| decide(s, &req.id, Decision::from_action("allow")?))
            .is_err());
        assert!(store.grants[0].once);
        assert_eq!(
            store.grants[0].allow_from,
            Some(vec![BodyId("server".into())])
        );
        let req = store
            .update(|s| upsert(s, BodyId("server".into()), "false"))
            .unwrap();
        let before = store.grants.clone();
        // Isolated failure: replacing state.json with a directory makes rename fail.
        std::fs::remove_file(dir.path().join("state.json")).unwrap();
        std::fs::create_dir(dir.path().join("state.json")).unwrap();
        assert!(store
            .update(|s| decide(s, &req.id, Decision::from_action("once")?))
            .is_err());
        assert_eq!(store.grants, before);
        assert!(store.requests.iter().any(|r| r.id == req.id));
        assert!(store.ensure_writable().is_err());
    }
}
