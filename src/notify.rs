use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::Request;
use notify_rust::{Hint, Notification, Timeout};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub type NotifyHook = Box<dyn Fn(&Request, &[&str]) + Send + Sync>;
static HOOK: Mutex<Option<NotifyHook>> = Mutex::new(None);
const ACTIONS: &[&str] = &["once", "allow", "deny"];
pub fn set_notify_hook(hook: Option<NotifyHook>) {
    *HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

/// Observe persisted requests, including requests present at daemon restart.
/// This future lives inside the daemon, so stopping it also stops delivery.
pub(crate) async fn run(store: Arc<Mutex<Store>>) {
    let mut observed = HashSet::new();
    let mut delivered = HashSet::new();
    loop {
        let pending = store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requests
            .clone();
        observed.retain(|id| pending.iter().any(|r| &r.id == id));
        delivered.retain(|id| pending.iter().any(|r| &r.id == id));
        for request in pending {
            if observed.insert(request.id.clone()) {
                if let Some(hook) = HOOK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    hook(&request, ACTIONS);
                }
            }
            if !delivered.contains(&request.id) && enabled() {
                let r = request.clone();
                let weak = Arc::downgrade(&store);
                match tokio::task::spawn_blocking(move || show_os(&r, weak)).await {
                    Ok(Ok(())) => {
                        delivered.insert(request.id);
                    }
                    Ok(Err(e)) => eprintln!("{e}; request remains in clix pending"),
                    Err(e) => eprintln!("notification worker failed: {e}"),
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
fn enabled() -> bool {
    std::env::var("CLIX_NOTIFY").as_deref() != Ok("0")
        && ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|k| std::env::var_os(k).is_some_and(|s| !s.is_empty()))
}
fn show_os(r: &Request, store: Weak<Mutex<Store>>) -> Result<()> {
    let mut n = Notification::new();
    n.summary("Clix")
        .body(&format!("{} wants {} on this machine", r.from, r.tool))
        .appname("clix")
        .hint(Hint::Resident(true))
        .timeout(Timeout::Never)
        .action("default", "Allow once")
        .action("once", "Allow once")
        .action("allow", "Allow")
        .action("deny", "Deny");
    let handle = n
        .show()
        .map_err(|e| ClixError::Io(format!("could not show Clix notification: {e}")))?;
    let id = r.id.clone();
    std::thread::spawn(move || {
        handle.wait_for_action(|action| {
            let action = if action == "default" { "once" } else { action };
            if !ACTIONS.contains(&action) {
                return;
            }
            if let Some(store) = store.upgrade() {
                let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = s.update(|s| crate::request::apply_action(s, &id, action)) {
                    eprintln!("could not apply notification action: {e}");
                }
            }
        });
    });
    Ok(())
}
