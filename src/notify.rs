use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::Request;
use notify_rust::{Hint, Notification, Timeout};
use std::collections::HashSet;
use std::path::Path;
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
    std::env::var("CLIX_NOTIFY").as_deref() != Ok("0") && display_present()
}

/// Whether a graphical session is present for OS notifications and the tray.
///
/// The daemon usually starts before the compositor sets DISPLAY/WAYLAND_DISPLAY
/// in the session environment, so a check that trusts only the process env stays
/// false for the whole session and requests reach the owner solely through
/// `clix pending`. Detect a live compositor or X server by its socket, which
/// appears at login regardless of what the daemon inherited, so a headless
/// server stays quiet while a desktop delivers.
pub(crate) fn display_present() -> bool {
    display_present_in(
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR")
            .as_deref()
            .map(Path::new),
        Path::new("/tmp/.X11-unix"),
    )
}

pub fn display_present_in(
    display: Option<&std::ffi::OsStr>,
    wayland: Option<&std::ffi::OsStr>,
    runtime_dir: Option<&Path>,
    x11_dir: &Path,
) -> bool {
    if [display, wayland]
        .iter()
        .any(|v| v.is_some_and(|s| !s.is_empty()))
    {
        return true;
    }
    // A Wayland compositor leaves a `wayland-<n>` socket (with a `.lock`
    // sibling) in XDG_RUNTIME_DIR while it runs.
    if let Some(dir) = runtime_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("wayland-") && !name.ends_with(".lock") {
                    return true;
                }
            }
        }
    }
    // An X server leaves a socket under /tmp/.X11-unix (X0, X1, …).
    std::fs::read_dir(x11_dir)
        .ok()
        .and_then(|mut e| e.next())
        .is_some()
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
            let Ok(decision) = crate::request::Decision::from_action(action) else {
                return;
            };
            if let Some(store) = store.upgrade() {
                let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = s.update(|s| crate::request::decide(s, &id, decision)) {
                    eprintln!("could not apply notification action: {e}");
                }
            }
        });
    });
    Ok(())
}
