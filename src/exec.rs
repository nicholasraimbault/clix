use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{ClixError, Result};
use crate::grant::{self, consume_once};
use crate::store::Store;
use crate::types::{BodyId, Grant};

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
    let grant = {
        let store = lock(store);
        match grant::check(&store, &argv[0], from) {
            Ok(g) => g,
            Err(e) => {
                return Ok(json!({
                    "status": "denied",
                    "reason": e.to_string(),
                }));
            }
        }
    };
    match run_granted(&grant, argv) {
        Err(e) => Ok(json!({
            "status": "denied",
            "reason": e.to_string(),
        })),
        Ok(out) => {
            let exit = out.status.code().unwrap_or(1);
            if out.status.success() {
                let mut store = lock(store);
                consume_once(&mut store, &grant.tool);
                store.save()?;
            }
            Ok(json!({
                "status": "done",
                "exit": exit,
                "stdout": String::from_utf8_lossy(&out.stdout),
                "stderr": String::from_utf8_lossy(&out.stderr),
            }))
        }
    }
}

fn lock(store: &Arc<Mutex<Store>>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}
