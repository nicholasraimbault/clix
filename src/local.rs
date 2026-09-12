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
use crate::exec::exec_checked;
use crate::grant;
use crate::job;
use crate::mesh::{self, MeshHandle};
use crate::pair::{self, hostname};
use crate::request;
use crate::store::Store;
use crate::types::{BodyId, Job, JobStatus, Schedule};

pub fn client_send(sock: &Path, req: Value) -> Result<Value> {
    let no_wait = req.get("no_wait").and_then(Value::as_bool).unwrap_or(false);
    let mut stream = UnixStream::connect(sock).map_err(connect_err)?;
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
        if v.get("status").and_then(Value::as_str) == Some("waiting") && !no_wait {
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
        "request" => rpc_request(store, &req).await,
        "pending" => rpc_pending(store),
        "allow" => rpc_allow(store, &req),
        "deny" => rpc_deny(store),
        other => Err(ClixError::Usage(format!("unknown op: {other}"))),
    }
}

fn lock_store(store: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

fn rpc_add(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    let tool = req
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| ClixError::Usage("usage: clix add <tool>".into()))?;
    let allow = json_string_list(req.get("allow"));
    let once = req.get("once").and_then(Value::as_bool).unwrap_or(false);
    let until = rpc_until(req)?;
    let schedule = rpc_schedule(req)?;
    let mut store = lock_store(store);
    let grant = grant::add(&mut store, tool, &allow, once, until, schedule)?;
    store.save()?;
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
    grant::remove(&mut store, tool)?;
    store.save()?;
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
        .ok_or_else(|| ClixError::Usage("usage: clix <body> <cmd>…".into()))?;
    let argv = json_string_list(req.get("argv"));
    if argv.is_empty() {
        return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
    }
    let no_wait = req.get("no_wait").and_then(Value::as_bool).unwrap_or(false);
    let (this, dest, sk) = {
        let store = lock_store(store);
        let dest = store.peers.iter().find(|p| p.name.0 == body).cloned();
        (store.body_name.clone(), dest, store.owner_sk.clone())
    };
    if body == this {
        let resp = exec_checked(store, &BodyId(this), &argv)?;
        write_json(writer, &resp).await?;
        return Ok(());
    }
    let Some(addr) = dest.and_then(|p| p.addr) else {
        write_json(
            writer,
            &json!({
                "status": "denied",
                "reason": format!("{body} is not this body"),
            }),
        )
        .await?;
        return Ok(());
    };
    if let Err(e) = crate::pin::sync_with_peer(store, &sk, &addr, body).await {
        if crate::pin::is_conflict(&e) {
            return Err(e);
        }
    }
    match mesh::call(&addr, &sk, json!({"op": "exec", "argv": argv})).await {
        Ok(resp) => {
            append_origin_job(store, &resp)?;
            write_json(writer, &resp).await?;
            Ok(())
        }
        Err(e) if job::is_unreachable(&e) => {
            let from = BodyId(this);
            let dest = BodyId(body.to_string());
            let waiting = job::append(store, from, dest, argv.clone(), JobStatus::WaitingBody)?;
            write_json(writer, &job::waiting_json(body, &waiting)).await?;
            let store = store.clone();
            let woke = mesh.woke.clone();
            let body = body.to_string();
            let sk = sk.clone();
            if no_wait {
                tokio::spawn(async move {
                    let _ = job::wait_for_peer(&store, &woke, &body, &sk, &argv, &waiting).await;
                });
                Ok(())
            } else {
                let resp = job::wait_for_peer(&store, &woke, &body, &sk, &argv, &waiting).await?;
                write_json(writer, &resp).await?;
                Ok(())
            }
        }
        Err(e) => Err(e),
    }
}

async fn rpc_request(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    let tool = req
        .get("tool")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ClixError::Usage("usage: clix request <body> <tool>".into()))?;
    let body = req.get("body").and_then(Value::as_str).unwrap_or("");
    let (this, dest, sk) = {
        let store = lock_store(store);
        let dest = store.peers.iter().find(|p| p.name.0 == body).cloned();
        (store.body_name.clone(), dest, store.owner_sk.clone())
    };
    if body.is_empty() || body == this {
        let mut store = lock_store(store);
        let r = request::upsert(&mut store, BodyId(this), tool)?;
        store.save()?;
        return Ok(json!({"ok": true, "request": r}));
    }
    let Some(addr) = dest.and_then(|p| p.addr) else {
        return Err(ClixError::Usage(format!("{body} is not this body")));
    };
    mesh::call(&addr, &sk, json!({"op": "request", "tool": tool})).await
}

fn rpc_pending(store: &Arc<Mutex<Store>>) -> Result<Value> {
    let store = lock_store(store);
    Ok(json!({"ok": true, "requests": request::pending(&store)}))
}

fn rpc_allow(store: &Arc<Mutex<Store>>, req: &Value) -> Result<Value> {
    let allow = json_string_list(req.get("allow"));
    let once = req.get("once").and_then(Value::as_bool).unwrap_or(true);
    let until = rpc_until(req)?;
    let schedule = rpc_schedule(req)?;
    let mut store = lock_store(store);
    let grant = request::allow(&mut store, &allow, once, until, schedule)?;
    store.save()?;
    Ok(json!({"ok": true, "tool": grant.tool, "once": grant.once}))
}

fn rpc_deny(store: &Arc<Mutex<Store>>) -> Result<Value> {
    let mut store = lock_store(store);
    request::deny(&mut store)?;
    store.save()?;
    Ok(json!({"ok": true}))
}

fn rpc_log(store: &Arc<Mutex<Store>>) -> Result<Value> {
    let store = lock_store(store);
    let lines: Vec<String> = store.jobs.iter().map(job::format_line).collect();
    Ok(json!({"jobs": store.jobs, "lines": lines}))
}

fn append_origin_job(store: &Arc<Mutex<Store>>, resp: &Value) -> Result<()> {
    let Some(v) = resp.get("job") else {
        return Ok(());
    };
    let job: Job = serde_json::from_value(v.clone())?;
    let mut store = lock_store(store);
    store.append_job(job)
}

fn rpc_status(store: &Arc<Mutex<Store>>, mesh: &MeshHandle) -> Result<Value> {
    let store = lock_store(store);
    Ok(json!({
        "body": store.body_name,
        "peers": store.peers,
        "mesh_addr": mesh.addr,
    }))
}

fn rpc_name(req: &Value) -> Option<&str> {
    req.get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn apply_body_name(store: &Arc<Mutex<Store>>, name: Option<&str>) -> Result<()> {
    let mut store = lock_store(store);
    if let Some(n) = name {
        store.body_name = n.to_string();
    }
    if store.body_name.is_empty() {
        store.body_name = hostname();
    }
    store.save()
}

fn rpc_pair_start(store: &Arc<Mutex<Store>>, mesh: &MeshHandle, req: &Value) -> Result<Value> {
    apply_body_name(store, rpc_name(req))?;
    let phrase = pair::phrase();
    let rx = mesh.register_pair();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    mesh.set_pair_done(done_rx);
    let store = store.clone();
    let p = phrase.clone();
    let local_addr = mesh.addr.clone();
    tokio::spawn(async move {
        let result = pair::complete_listen(store, rx, &p, &local_addr).await;
        let _ = done_tx.send(result);
    });
    Ok(json!({
        "ok": true,
        "phrase": phrase,
        "addr": mesh.addr,
    }))
}

async fn rpc_pair_join(store: &Arc<Mutex<Store>>, mesh: &MeshHandle, req: &Value) -> Result<Value> {
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
        Some(addr) => pair::pair_join(store.clone(), addr, phrase, &mesh.addr).await?,
        None => pair::pair_join_any(store.clone(), phrase, &mesh.addr).await?,
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
            let sk = lock_store(store).owner_sk.clone();
            crate::pin::sync_after_pair(store, &sk, &peer).await?;
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
        return Ok(Some(SystemTime::now() + Duration::from_secs(secs)));
    }
    if let Some(secs) = req.get("until").and_then(Value::as_u64) {
        return Ok(Some(UNIX_EPOCH + Duration::from_secs(secs)));
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
            "tool": tool,
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
        Cmd::Remove { tool } => Ok(json!({"op": "remove", "tool": tool})),
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
            "op": "allow",
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
        Cmd::Deny => Ok(json!({"op": "deny"})),
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
                    let from = r.get("from").and_then(Value::as_str).unwrap_or("?");
                    let tool = r.get("tool").and_then(Value::as_str).unwrap_or("?");
                    println!("{from} wants {tool}");
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
            if status == Some("denied") || status == Some("failed") {
                let reason = v.get("reason").and_then(Value::as_str).unwrap_or("denied");
                return Err(ClixError::Io(reason.to_string()));
            }
            if let Some(s) = v.get("stdout").and_then(Value::as_str) {
                print!("{s}");
            }
            if let Some(s) = v.get("stderr").and_then(Value::as_str) {
                eprint!("{s}");
            }
            Ok(())
        }
        Cmd::Status => {
            if let Some(body) = v.get("body").and_then(Value::as_str) {
                println!("{body}");
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
