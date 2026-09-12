use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Weekday;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};

use crate::cli::{parse_ampm_naive, Cmd};
use crate::error::{ClixError, Result};
use crate::exec;
use crate::grant;
use crate::job;
use crate::mesh::MeshHandle;
use crate::pair;
use crate::request;
use crate::store::Store;
use crate::types::{BodyId, JobStatus, Schedule};

pub fn client_send(sock: &Path, req: Value) -> Result<Value> {
    client_send_timeout(sock, req, None)
}

pub(crate) fn client_send_timeout(
    sock: &Path,
    req: Value,
    timeout: Option<Duration>,
) -> Result<Value> {
    let no_wait = req.get("no_wait").and_then(Value::as_bool).unwrap_or(false);
    let mut stream = UnixStream::connect(sock).map_err(connect_err)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let mut payload = serde_json::to_string(&req)?;
    payload.push('\n');
    stream.write_all(payload.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(ClixError::Io("daemon closed the connection".into()));
        }
        let v: Value = serde_json::from_str(&line)?;
        if v.get("ok") == Some(&Value::Bool(false)) {
            let msg = v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            return Err(ClixError::Io(msg.to_string()));
        }
        if req["op"] == "exec"
            && v.get("status").and_then(Value::as_str) == Some("waiting")
            && !no_wait
        {
            if let Some(msg) = v.get("waiting").and_then(Value::as_str) {
                eprintln!("{msg}");
            }
            continue;
        }
        return Ok(v);
    }
}

fn connect_err(e: std::io::Error) -> ClixError {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => ClixError::NoDaemon,
        _ => ClixError::Io(e.to_string()),
    }
}

pub(crate) async fn handle_connection(
    store: Arc<Mutex<Store>>,
    mesh: MeshHandle,
    stream: tokio::net::UnixStream,
) {
    let cred = match stream.peer_cred() {
        Ok(c) => c,
        Err(_) => return,
    };
    if cred.uid() != nix::unistd::Uid::current().as_raw() {
        return;
    }
    let (reader, mut writer) = stream.into_split();
    let mut lines = AsyncBufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                if write_json(&mut writer, &json!({"ok": false, "error": e.to_string()}))
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };
        let op = req.get("op").and_then(Value::as_str);
        if op == Some("exec") {
            if let Err(e) = rpc_exec(&store, &mesh, &req, &mut writer).await {
                let err = json!({"ok": false, "error": e.to_string()});
                if write_json(&mut writer, &err).await.is_err() {
                    break;
                }
            }
            continue;
        }
        let resp = match handle_rpc(&store, &mesh, req).await {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        };
        if write_json(&mut writer, &resp).await.is_err() {
            break;
        }
    }
}

async fn write_json<W: AsyncWriteExt + Unpin>(writer: &mut W, v: &Value) -> std::io::Result<()> {
    let mut s = serde_json::to_vec(v)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    s.push(b'\n');
    writer.write_all(&s).await?;
    writer.flush().await
}

async fn handle_rpc(store: &Arc<Mutex<Store>>, mesh: &MeshHandle, req: Value) -> Result<Value> {
    let op = req
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Usage("missing op".into()))?;
    match op {
        "add" => rpc_add(store, &req),
        "hands" => rpc_hands(store),
        "remove" => rpc_remove(store, &req),
        "log" => rpc_log(store),
        "status" => rpc_status(store, mesh),
        "pair_start" => rpc_pair_start(store, mesh, &req),
        "pair_join" => rpc_pair_join(store, mesh, &req).await,
        "pair_await" => rpc_pair_await(store, mesh).await,
        "request" => rpc_request(store, mesh, &req).await,
        "pending" => rpc_pending(store),
        "allow_request" => rpc_allow(store, &req),
        "deny_request" => rpc_deny(store, &req),
        "allow" | "deny" => Err(ClixError::Usage(
            "update the Clix client and select a request ID from clix pending".into(),
        )),
        other => Err(ClixError::Usage(format!("unknown op: {other}"))),
    }
}

fn lock_store(store: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

fn validate_grant_fields(req: &Value) -> Result<()> {
    for key in ["once", "weekdays"] {
        if req.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(ClixError::Usage(format!("{key} must be a boolean")));
        }
    }
    for key in ["allow", "days"] {
        if req.get(key).is_some_and(|v| {
            !v.is_null() && !v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
        }) {
            return Err(ClixError::Usage(format!("{key} must be a list of strings")));
        }
    }
    if req.get("dates").is_some_and(|v| {
        !v.is_null()
            && !v.as_array().is_some_and(|a| {
                a.iter()
                    .all(|v| v.as_u64().is_some_and(|n| (1..=31).contains(&n)))
            })
    }) {
        return Err(ClixError::Usage(
            "dates must be a list of dates 1–31".into(),
        ));
    }
    for key in ["for_secs", "until"] {
        if req
            .get(key)
            .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
        {
            return Err(ClixError::Usage(format!(
                "{key} must be a nonnegative integer"
            )));
        }
    }
    for key in ["from", "to"] {
        if req.get(key).is_some_and(|v| !v.is_null() && !v.is_string()) {
            return Err(ClixError::Usage(format!("{key} must be a time")));
        }
    }
    Ok(())
}

fn rpc_add(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    validate_grant_fields(req)?;
    let tool = req
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Usage("usage: clix add <tool>".into()))?;
    let allow = json_string_list(req.get("allow"));
    let once = req.get("once").and_then(Value::as_bool).unwrap_or(false);
    let until = rpc_until(req)?;
    let schedule = rpc_schedule(req)?;
    let mut store = lock_store(store);
    let grant = store.update(|s| grant::add(s, tool, &allow, once, until, schedule))?;
    Ok(json!({"ok": true, "tool": grant.tool}))
}

fn rpc_hands(store: &Arc<Mutex<Store>>) -> Result<Value> {
    let store = lock_store(store);
    Ok(json!({"hands": grant::hands(&store)}))
}

fn rpc_remove(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    let tool = req
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Usage("usage: clix remove <tool>".into()))?;
    let mut store = lock_store(store);
    store.update(|s| grant::remove(s, tool))?;
    Ok(json!({"ok": true}))
}

async fn rpc_exec<W: AsyncWriteExt + Unpin>(
    store: &Arc<Mutex<Store>>,
    mesh: &MeshHandle,
    req: &Value,
    writer: &mut W,
) -> Result<()> {
    let body = req
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Usage("missing body".into()))?;
    let argv: Vec<String> =
        serde_json::from_value(req.get("argv").cloned().unwrap_or(Value::Null))?;
    if argv.is_empty() {
        return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
    }
    let no_wait = req.get("no_wait").and_then(Value::as_bool).unwrap_or(false);
    let this = lock_store(store).body_name.clone();
    let id = if body == this {
        let id = job::new_id()?;
        exec::submit(store, &BodyId(this), &argv, id.clone())?;
        id
    } else {
        if !lock_store(store).peers.iter().any(|p| p.name.0 == body) {
            return Err(ClixError::Usage(format!("{body} is not a paired body")));
        }
        job::append(
            store,
            BodyId(this),
            BodyId(body.into()),
            argv,
            JobStatus::Queued,
        )?
        .id
    };
    mesh.wake();
    let mut waiting_reported = false;
    loop {
        lock_store(store).ensure_writable()?;
        let j = lock_store(store)
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .ok_or_else(|| ClixError::Protocol("accepted job disappeared".into()))?;
        if job::is_terminal(&j.status) || (no_wait && !matches!(j.status, JobStatus::Queued)) {
            write_json(writer, &job::exec_json(&j)).await?;
            return Ok(());
        }
        if matches!(j.status, JobStatus::WaitingBody) && !waiting_reported {
            write_json(writer, &job::waiting_json(body, &j)).await?;
            waiting_reported = true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn rpc_request(store: &Arc<Mutex<Store>>, mesh: &MeshHandle, req: &Value) -> Result<Value> {
    let tool = req
        .get("tool")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ClixError::Usage("usage: clix request <body> <tool>".into()))?;
    let body = req.get("body").and_then(Value::as_str).unwrap_or("");
    let (this, dest) = {
        let store = lock_store(store);
        let dest = store.peers.iter().find(|p| p.name.0 == body).cloned();
        (store.body_name.clone(), dest)
    };
    if body.is_empty() || body == this {
        let mut store = lock_store(store);
        let r = store.update(|s| request::upsert(s, BodyId(this), tool))?;
        return Ok(json!({"ok": true, "request": r}));
    }
    let peer = dest.ok_or_else(|| ClixError::Usage(format!("{body} is not a paired body")))?;
    let id = job::new_id()?;
    lock_store(store).update(|s| {
        s.outbound_requests.push(crate::types::OutboundRequest {
            id: id.clone(),
            body: peer.name,
            tool: tool.into(),
            waiting: false,
            error: None,
        });
        Ok(())
    })?;
    mesh.wake();
    loop {
        lock_store(store).ensure_writable()?;
        let pending = lock_store(store)
            .outbound_requests
            .iter()
            .find(|r| r.id == id)
            .cloned();
        match pending {
            None => return Ok(json!({"ok":true,"status":"delivered","request_id":id})),
            Some(r) => {
                if let Some(error) = r.error {
                    return Err(ClixError::Protocol(error));
                }
                if r.waiting {
                    return Ok(json!({"ok":true,"status":"waiting","body":body,"request_id":id}));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn rpc_pending(store: &Arc<Mutex<Store>>) -> Result<Value> {
    let store = lock_store(store);
    Ok(json!({"ok": true, "requests": request::pending(&store)}))
}

fn rpc_allow(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    let id = request_id(req)?;
    validate_grant_fields(req)?;
    let allow = json_string_list(req.get("allow"));
    let once = req.get("once").and_then(Value::as_bool).unwrap_or(true);
    let until = rpc_until(req)?;
    let schedule = rpc_schedule(req)?;
    let mut store = lock_store(store);
    let scope = if allow.is_empty() {
        request::Scope::Requester
    } else {
        request::Scope::Bodies(allow)
    };
    let grant = store.update(|s| {
        request::decide(
            s,
            id,
            request::Decision::Allow {
                scope,
                once,
                until,
                schedule,
            },
        )
    })?;
    Ok(json!({"ok": true, "request_id": id, "grant": grant}))
}

fn request_id(req: &Value) -> Result<&str> {
    req.get("request_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ClixError::Usage("provide a request ID from clix pending".into()))
}

fn rpc_deny(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    let id = request_id(req)?;
    let mut store = lock_store(store);
    store.update(|s| request::decide(s, id, request::Decision::Deny))?;
    Ok(json!({"ok": true, "request_id": id}))
}

fn rpc_log(store: &Arc<Mutex<Store>>) -> Result<Value> {
    let store = lock_store(store);
    let lines: Vec<String> = store.jobs.iter().map(job::format_line).collect();
    Ok(json!({"jobs": store.jobs, "lines": lines}))
}

fn rpc_status(store: &Arc<Mutex<Store>>, mesh: &MeshHandle) -> Result<Value> {
    let store = lock_store(store);
    Ok(json!({
        "body": store.body_name,
        "peers": store.peers,
        "mesh_addr": mesh.addr(),
        "outbound_requests": store.outbound_requests,
    }))
}

fn rpc_name(req: &Value) -> Option<&str> {
    req.get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn apply_body_name(store: &Arc<Mutex<Store>>, name: Option<&str>) -> Result<()> {
    let mut s = lock_store(store);
    let name = name.unwrap_or(&s.body_name).to_string();
    pair::validate_name(&name)?;
    if name != s.body_name && (!s.peers.is_empty() || !s.jobs.is_empty() || !s.grants.is_empty()) {
        return Err(ClixError::Usage(
            "cannot rename a body after pairing or granting tools".into(),
        ));
    }
    s.update(|s| {
        s.body_name = name;
        pair::validate_store(s)
    })
}

fn rpc_pair_start(store: &Arc<Mutex<Store>>, mesh: &MeshHandle, req: &Value) -> Result<Value> {
    if mesh.addr().is_empty() {
        return Err(ClixError::Usage(
            "Tailscale is off. Start it, then pair.".into(),
        ));
    }
    apply_body_name(store, rpc_name(req))?;
    let phrase = pair::phrase();
    let rx = mesh.register_pair();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    mesh.set_pair_done(done_rx);
    let store = store.clone();
    let p = phrase.clone();
    let local_addr = mesh.addr();
    tokio::spawn(async move {
        let result = pair::complete_listen(store, rx, &p, &local_addr).await;
        let _ = done_tx.send(result);
    });
    Ok(json!({
        "ok": true,
        "phrase": phrase,
        "addr": mesh.addr(),
    }))
}

async fn rpc_pair_join(store: &Arc<Mutex<Store>>, mesh: &MeshHandle, req: &Value) -> Result<Value> {
    if mesh.addr().is_empty() {
        return Err(ClixError::Usage(
            "Tailscale is off. Start it, then pair.".into(),
        ));
    }
    apply_body_name(store, rpc_name(req))?;
    let phrase = req
        .get("phrase")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Usage("usage: clix pair <phrase>".into()))?;
    let addr = req
        .get("addr")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let peer = match addr {
        Some(addr) => pair::pair_join(store.clone(), addr, phrase, &mesh.addr()).await?,
        None => pair::pair_join_any(store.clone(), phrase, &mesh.addr()).await?,
    };
    Ok(json!({
        "ok": true,
        "name": peer.name.0,
    }))
}

async fn rpc_pair_await(store: &Arc<Mutex<Store>>, mesh: &MeshHandle) -> Result<Value> {
    let rx = mesh.take_pair_done()?;
    match rx.await {
        Ok(Ok(peer)) => {
            pair::pin_after_pair(store, &peer).await?;
            Ok(json!({"ok": true, "name": peer.name.0}))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ClixError::Io("pair cancelled".into())),
    }
}

fn json_string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn rpc_until(req: &Value) -> Result<Option<SystemTime>> {
    if let Some(secs) = req.get("for_secs").and_then(Value::as_u64) {
        return Ok(Some(
            SystemTime::now()
                .checked_add(Duration::from_secs(secs))
                .ok_or_else(|| ClixError::Usage("grant duration is out of range".into()))?,
        ));
    }
    if let Some(secs) = req.get("until").and_then(Value::as_u64) {
        return Ok(Some(
            UNIX_EPOCH
                .checked_add(Duration::from_secs(secs))
                .ok_or_else(|| ClixError::Usage("grant expiry is out of range".into()))?,
        ));
    }
    Ok(None)
}

fn rpc_schedule(req: &Value) -> Result<Option<Schedule>> {
    let weekdays = req
        .get("weekdays")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let day_vals = req.get("days").and_then(Value::as_array);
    if weekdays && day_vals.is_some_and(|a| !a.is_empty()) {
        return Err(ClixError::Usage(
            "use --days or --weekdays, not both".into(),
        ));
    }
    let mut days = Vec::new();
    if weekdays {
        days.extend([
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
        ]);
    }
    if let Some(arr) = day_vals {
        for d in arr {
            let s = d
                .as_str()
                .ok_or_else(|| ClixError::Usage("unknown weekday (use mon..sun)".into()))?;
            days.push(parse_weekday(s)?);
        }
    }
    let mut dates = Vec::new();
    if let Some(arr) = req.get("dates").and_then(Value::as_array) {
        for d in arr {
            let n = d
                .as_u64()
                .ok_or_else(|| ClixError::Usage(format!("bad date '{d}' (use 1–31)")))?;
            if !(1..=31).contains(&n) {
                return Err(ClixError::Usage(format!("bad date '{n}' (use 1–31)")));
            }
            dates.push(n as u8);
        }
    }
    let from = match req.get("from").and_then(Value::as_str) {
        Some(s) => Some(parse_ampm_naive(s)?),
        None => None,
    };
    let to = match req.get("to").and_then(Value::as_str) {
        Some(s) => Some(parse_ampm_naive(s)?),
        None => None,
    };
    if days.is_empty() && dates.is_empty() && from.is_none() && to.is_none() {
        return Ok(None);
    }
    Ok(Some(Schedule {
        days,
        dates,
        from,
        to,
    }))
}

fn parse_weekday(s: &str) -> Result<Weekday> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mon" => Ok(Weekday::Mon),
        "tue" => Ok(Weekday::Tue),
        "wed" => Ok(Weekday::Wed),
        "thu" => Ok(Weekday::Thu),
        "fri" => Ok(Weekday::Fri),
        "sat" => Ok(Weekday::Sat),
        "sun" => Ok(Weekday::Sun),
        _ => Err(ClixError::Usage(format!(
            "unknown weekday '{s}' (use mon..sun)"
        ))),
    }
}

pub(crate) fn rpc_from_cmd(cmd: &Cmd) -> Result<Value> {
    match cmd {
        Cmd::Add {
            tool,
            allow,
            once,
            for_dur,
            until,
            days,
            dates,
            from,
            to,
            weekdays,
        } => Ok(json!({
            "op": "add",
            "tool": crate::grant::resolve_tool(tool)?,
            "allow": allow,
            "once": once,
            "for_secs": for_dur.map(|d| d.as_secs()),
            "until": until.map(system_time_secs),
            "days": days,
            "dates": dates,
            "from": from,
            "to": to,
            "weekdays": weekdays,
        })),
        Cmd::Hands => Ok(json!({"op": "hands"})),
        Cmd::Remove { tool } => {
            Ok(json!({"op": "remove", "tool": crate::grant::removal_target(tool)?}))
        }
        Cmd::Exec {
            body,
            argv,
            no_wait,
        } => Ok(json!({
            "op": "exec",
            "body": body,
            "argv": argv,
            "no_wait": no_wait,
        })),
        Cmd::Pair { phrase, name } => match phrase {
            None => Ok(json!({"op": "pair_start", "name": name})),
            Some(phrase) => Ok(json!({"op": "pair_join", "phrase": phrase, "name": name})),
        },
        Cmd::Log => Ok(json!({"op": "log"})),
        Cmd::Pending => Ok(json!({"op": "pending"})),
        Cmd::Allow {
            request_id,
            allow,
            once,
            for_dur,
            until,
            days,
            dates,
            from,
            to,
            weekdays,
        } => Ok(json!({
            "op": "allow_request",
            "request_id": request_id,
            "allow": allow,
            "once": once,
            "for_secs": for_dur.map(|d| d.as_secs()),
            "until": until.map(system_time_secs),
            "days": days,
            "dates": dates,
            "from": from,
            "to": to,
            "weekdays": weekdays,
        })),
        Cmd::Deny { request_id } => Ok(json!({"op": "deny_request", "request_id": request_id})),
        Cmd::Request { body, tool } => Ok(json!({
            "op": "request",
            "body": body,
            "tool": tool,
        })),
        Cmd::Status => Ok(json!({"op": "status"})),
        Cmd::Daemon | Cmd::Install => Err(ClixError::Usage("not a client command".into())),
    }
}

pub(crate) fn emit_rpc(cmd: &Cmd, v: &Value) -> Result<()> {
    match cmd {
        Cmd::Hands => {
            if let Some(arr) = v.get("hands").and_then(Value::as_array) {
                for h in arr {
                    if let Some(tool) = h.get("tool").and_then(Value::as_str) {
                        println!("{tool}");
                    }
                }
            }
            Ok(())
        }
        Cmd::Log => {
            if let Some(arr) = v.get("lines").and_then(Value::as_array) {
                for line in arr {
                    if let Some(s) = line.as_str() {
                        println!("{s}");
                    }
                }
            }
            Ok(())
        }
        Cmd::Pending => {
            if let Some(arr) = v.get("requests").and_then(Value::as_array) {
                for r in arr {
                    let id = r.get("id").and_then(Value::as_str).unwrap_or("?");
                    let from = r.get("from").and_then(Value::as_str).unwrap_or("?");
                    let tool = r.get("tool").and_then(Value::as_str).unwrap_or("?");
                    println!("{id}  {from} wants {tool}");
                }
            }
            Ok(())
        }
        Cmd::Exec { no_wait, .. } => {
            let status = v.get("status").and_then(Value::as_str);
            if status == Some("waiting") {
                if let Some(msg) = v.get("waiting").and_then(Value::as_str) {
                    eprintln!("{msg}");
                }
                if *no_wait {
                    if let Some(id) = v
                        .get("job")
                        .and_then(|j| j.get("id"))
                        .and_then(Value::as_str)
                    {
                        println!("{id}");
                    }
                }
                return Ok(());
            }
            if matches!(status, Some("denied" | "failed" | "uncertain")) {
                let reason = v.get("reason").and_then(Value::as_str).unwrap_or("denied");
                return Err(ClixError::Io(reason.to_string()));
            }
            if matches!(status, Some("running" | "queued")) && *no_wait {
                if let Some(id) = v["job"]["id"].as_str() {
                    println!("{id}");
                }
                return Ok(());
            }
            let stdout: Vec<u8> =
                serde_json::from_value(v["job"].get("stdout").cloned().unwrap_or(json!([])))?;
            let stderr: Vec<u8> =
                serde_json::from_value(v["job"].get("stderr").cloned().unwrap_or(json!([])))?;
            std::io::stdout().lock().write_all(&stdout)?;
            std::io::stderr().lock().write_all(&stderr)?;
            let exit =
                v.get("exit").and_then(Value::as_i64).ok_or_else(|| {
                    ClixError::Protocol("completed command has no exit status".into())
                })? as i32;
            if exit != 0 {
                return Err(ClixError::ToolExit(exit));
            }
            Ok(())
        }
        Cmd::Status => {
            if let Some(body) = v.get("body").and_then(Value::as_str) {
                println!("{body}");
            }
            let addr = v["mesh_addr"].as_str().filter(|s| !s.is_empty());
            println!("mesh: {}", addr.unwrap_or("offline"));
            if let Some(peers) = v["peers"].as_array() {
                for peer in peers {
                    println!(
                        "paired: {} ({})",
                        peer["name"].as_str().unwrap_or("?"),
                        peer["addr"].as_str().unwrap_or("address unknown")
                    );
                }
            }
            if let Some(requests) = v["outbound_requests"].as_array() {
                for r in requests {
                    println!(
                        "request {}: {} wants {} ({})",
                        r["id"].as_str().unwrap_or("?"),
                        r["body"].as_str().unwrap_or("?"),
                        r["tool"].as_str().unwrap_or("?"),
                        r["error"].as_str().unwrap_or("waiting for delivery")
                    );
                }
            }
            Ok(())
        }
        Cmd::Request { body, tool } => {
            if v["status"] == "waiting" {
                eprintln!("{body} is unreachable; request for {tool} is saved and will be delivered when it returns.");
            }
            Ok(())
        }
        Cmd::Pair { phrase: None, .. } => {
            if let Some(p) = v.get("phrase").and_then(Value::as_str) {
                println!("pair with: {p}");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn system_time_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
