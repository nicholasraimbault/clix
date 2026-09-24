//! Pin inspection and explicit recovery through the local owner socket.
//! No mesh operation dispatches these commands.
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::cli::{PinCommand, RecoveryCommand};
use crate::error::{ClixError, Result};
use crate::{pin, Store};

fn field<'a>(request: &'a Value, name: &str) -> Result<&'a str> {
    request
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ClixError::Usage(format!("missing {name}")))
}

pub(crate) fn request(command: &PinCommand) -> Value {
    match command {
        PinCommand::On => json!({"op":"pin_on"}),
        PinCommand::Off => json!({"op":"pin_off"}),
        PinCommand::Sync { body } => json!({"op":"pin_sync","body":body}),
        PinCommand::Conflicts { body } => json!({"op":"pin_conflicts","body":body}),
        PinCommand::TakePeer { body, path, token } => {
            json!({"op":"pin_take_peer","body":body,"path":path,"token":token})
        }
        PinCommand::Recovery { command } => match command {
            RecoveryCommand::List { after } => json!({"op":"pin_recovery_list","after":after}),
            RecoveryCommand::Inspect { id } => json!({"op":"pin_recovery_inspect","id":id}),
            RecoveryCommand::Export { id, version, token } => {
                json!({"op":"pin_recovery_export","id":id,"version":version,"token":token})
            }
            RecoveryCommand::Resolve { id, choice, token } => {
                json!({"op":"pin_recovery_resolve","id":id,"choice":choice.replace('-',"_"),"token":token})
            }
            RecoveryCommand::Discard { id, token } => {
                json!({"op":"pin_recovery_discard","id":id,"token":token})
            }
        },
    }
}

pub(crate) async fn rpc(store: &Arc<Mutex<Store>>, request: Value) -> Result<Value> {
    match field(&request, "op")? {
        op @ ("pin_on" | "pin_off") => {
            let enabled = op == "pin_on";
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            if enabled {
                // Turning pin on prepares the tree it opts into, as the first
                // sync would; pairing no longer creates it while pin is off.
                std::fs::create_dir_all(pin::pin_root(&s))?;
            }
            s.update(|s| {
                s.pin_enabled = enabled;
                Ok(())
            })?;
            Ok(json!({"ok":true,"pin_enabled":enabled,"body":s.body_name}))
        }
        "pin_sync" => Ok(serde_json::to_value(
            pin::owner_pin_sync(store, field(&request, "body")?).await?,
        )?),
        "pin_conflicts" => Ok(serde_json::to_value(
            pin::inspect_peer_conflicts(store, field(&request, "body")?).await?,
        )?),
        "pin_take_peer" => {
            let report = pin::inspect_peer_conflicts(store, field(&request, "body")?).await?;
            let path = field(&request, "path")?;
            let token = field(&request, "token")?;
            let snapshot = report
                .conflicts
                .iter()
                .find(|c| c.path == path && c.token == token)
                .ok_or_else(|| {
                    ClixError::Usage(
                        "pin conflict changed; inspect conflicts again before choosing a version"
                            .into(),
                    )
                })?;
            pin::take_peer_conflict(store, snapshot).await?;
            Ok(
                json!({"ok":true,"path":path,"message":"Peer version adopted on this machine; displaced local data is retained. Run pin sync to check convergence."}),
            )
        }
        "pin_recovery_list" | "pin_recovery_inspect" => {
            let context =
                pin::RecoveryContext::from_store(&store.lock().unwrap_or_else(|e| e.into_inner()));
            tokio::task::spawn_blocking(move || match field(&request, "op")? {
                "pin_recovery_list" => {
                    if request
                        .get("after")
                        .is_some_and(|v| !v.is_null() && !v.is_string())
                    {
                        return Err(ClixError::Usage("after must be a recovery ID".into()));
                    }
                    Ok(serde_json::to_value(context.list_page(
                        request["after"].as_str(),
                        pin::MAX_RECOVERY_PAGE,
                    )?)?)
                }
                _ => Ok(serde_json::to_value(
                    context.inspect(field(&request, "id")?)?,
                )?),
            })
            .await
            .map_err(|e| ClixError::Io(format!("pin inspection failed: {e}")))?
        }
        _ => {
            let store = store.clone();
            tokio::task::spawn_blocking(move || {
                let store=store.lock().unwrap_or_else(|e|e.into_inner());
                match field(&request,"op")? {
                    "pin_recovery_resolve" => {
                        let choice=serde_json::from_value(request.get("choice").cloned().unwrap_or(Value::Null))?;
                        Ok(serde_json::to_value(pin::recovery_resolve(&store,field(&request,"id")?,field(&request,"token")?,choice)?)?)
                    }
                    "pin_recovery_discard" => {
                        pin::recovery_discard(&store,field(&request,"id")?,field(&request,"token")?)?;
                        Ok(json!({"ok":true,"message":"Inspected retained data discarded; live pins are preserved."}))
                    }
                    _ => Err(ClixError::Usage("unknown pin owner operation".into())),
                }
            }).await.map_err(|e|ClixError::Io(format!("pin owner task failed: {e}")))?
        }
    }
}

pub(crate) async fn export<W: AsyncWriteExt + Unpin>(
    store: &Arc<Mutex<Store>>,
    request: &Value,
    writer: &mut W,
) -> Result<()> {
    let id = field(request, "id")?.to_owned();
    let token = field(request, "token")?.to_owned();
    let version = serde_json::from_value(request.get("version").cloned().unwrap_or(Value::Null))?;
    let context =
        pin::RecoveryContext::from_store(&store.lock().unwrap_or_else(|e| e.into_inner()));
    let mut export = tokio::task::spawn_blocking(move || context.export(&id, version, &token))
        .await
        .map_err(|e| ClixError::Io(format!("pin export failed: {e}")))??;
    loop {
        let (next, chunk) = tokio::task::spawn_blocking(move || {
            let chunk = export.read_chunk(65536).and_then(|chunk| {
                if chunk.eof {
                    export.finish()?;
                }
                Ok(chunk)
            });
            (export, chunk)
        })
        .await
        .map_err(|e| ClixError::Io(format!("pin export failed: {e}")))?;
        export = next;
        let chunk = chunk?;
        crate::local::write_json(writer, &json!({"ok":true,"chunk":chunk})).await?;
        if chunk.eof {
            return Ok(());
        }
    }
}

pub(crate) fn client_export(socket: &Path, request: Value) -> Result<()> {
    let mut stream = UnixStream::connect(socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => ClixError::NoDaemon,
        _ => e.into(),
    })?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(crate::limits::IO_TIMEOUT))?;
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    let mut reader = BufReader::new(stream);
    let mut offset = 0u64;
    let mut total = None;
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = Vec::new();
        reader
            .by_ref()
            .take(512 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if line.len() > 512 * 1024 || line.last() != Some(&b'\n') {
            return Err(ClixError::Protocol(
                "pin export ended without a verified complete stream".into(),
            ));
        }
        let response: Value = serde_json::from_slice(&line)?;
        if response["ok"] != true {
            return Err(ClixError::Io(
                response["error"]
                    .as_str()
                    .unwrap_or("pin export failed")
                    .into(),
            ));
        }
        let chunk: pin::RecoveryChunk = serde_json::from_value(response["chunk"].clone())?;
        if chunk.offset != offset
            || chunk.bytes.len() > 65536
            || total.is_some_and(|n| n != chunk.total_bytes)
        {
            return Err(ClixError::Protocol(
                "pin export stream boundary changed".into(),
            ));
        }
        total = Some(chunk.total_bytes);
        offset = offset
            .checked_add(chunk.bytes.len() as u64)
            .ok_or_else(|| ClixError::Protocol("pin export size overflow".into()))?;
        if offset > chunk.total_bytes
            || (chunk.eof && offset != chunk.total_bytes)
            || (!chunk.eof && chunk.bytes.is_empty())
        {
            return Err(ClixError::Protocol(
                "pin export stream length is inconsistent".into(),
            ));
        }
        output.write_all(&chunk.bytes)?;
        if chunk.eof {
            output.flush()?;
            return Ok(());
        }
    }
}

pub(crate) fn emit(response: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(response)?);
    if response.get("synced") == Some(&Value::Bool(false)) {
        return Err(ClixError::Usage(
            response["error"]
                .as_str()
                .unwrap_or("pin synchronization did not complete")
                .into(),
        ));
    }
    Ok(())
}
