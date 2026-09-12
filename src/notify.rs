use std::env;
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use notify_rust::{Hint, Notification, Timeout};

use crate::error::Result;
use crate::request;
use crate::store::Store;
use crate::types::Request;

/// Test seam. Production still calls the OS unless `CLIX_NOTIFY=0`.
pub type NotifyHook = Box<dyn Fn(&Request, &[&str]) + Send + Sync>;

const TITLE: &str = "Clix";
const ACTIONS: &[&str] = &["once", "allow", "deny"];

static HOOK: Mutex<Option<NotifyHook>> = Mutex::new(None);
static STORES: Mutex<Vec<Weak<Mutex<Store>>>> = Mutex::new(Vec::new());

pub fn set_notify_hook(hook: Option<NotifyHook>) {
    *lock_hook() = hook;
}

pub(crate) fn bind_store(store: Arc<Mutex<Store>>) {
    let mut g = STORES.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|w| w.strong_count() > 0);
    let weak = Arc::downgrade(&store);
    if !g.iter().any(|w| Weak::ptr_eq(w, &weak)) {
        g.push(weak);
    }
}

fn lock_hook() -> std::sync::MutexGuard<'static, Option<NotifyHook>> {
    HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Title `Clix`, body `{from} wants {tool} on this machine`, actions once/allow/deny.
pub fn notify_request(r: &Request) -> Result<()> {
    if let Some(hook) = lock_hook().as_ref() {
        hook(r, ACTIONS);
    }
    if !os_notify_enabled() {
        return Ok(());
    }
    let req = r.clone();
    thread::spawn(move || {
        let _ = show_os(&req);
    });
    Ok(())
}

fn os_notify_enabled() -> bool {
    match env::var("CLIX_NOTIFY") {
        Ok(v) if v == "0" => false,
        _ => has_display(),
    }
}

fn has_display() -> bool {
    env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
        || env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

fn body_text(r: &Request) -> String {
    format!("{} wants {} on this machine", r.from, r.tool)
}

fn show_os(r: &Request) -> Result<()> {
    let body = body_text(r);
    let mut n = Notification::new();
    n.summary(TITLE)
        .body(&body)
        .appname("clix")
        .hint(Hint::Resident(true))
        .timeout(Timeout::Never);
    for id in ACTIONS {
        let label = match *id {
            "once" => "Once",
            "allow" => "Allow",
            "deny" => "Deny",
            other => other,
        };
        n.action(id, label);
    }
    let handle = match n.show() {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };
    handle.wait_for_action(|action| {
        apply_clicked(action, r);
    });
    Ok(())
}

fn apply_clicked(action: &str, r: &Request) {
    if !matches!(action, "once" | "allow" | "deny") {
        return;
    }
    for store in bound_stores() {
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        if s.requests.iter().any(|x| x.id == r.id) {
            if request::apply_action(&mut s, &r.id, action).is_ok() {
                let _ = s.save();
            }
            return;
        }
    }
}

fn bound_stores() -> Vec<Arc<Mutex<Store>>> {
    let mut g = STORES.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|w| w.strong_count() > 0);
    g.iter().filter_map(|w| w.upgrade()).collect()
}
