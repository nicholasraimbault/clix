use std::env;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ksni::menu::{StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip, TrayMethods};

use crate::request;
use crate::store::Store;
use crate::types::JobStatus;

/// Idle: `Clix is running`. Pending: `1 request`. Running job: `{tool} from {body}`.
pub fn tray_tooltip(store: &Store) -> String {
    match store.requests.len() {
        0 => {}
        1 => return "1 request".into(),
        n => return format!("{n} requests"),
    }
    if let Some(job) = store
        .jobs
        .iter()
        .rev()
        .find(|j| matches!(j.status, JobStatus::Running))
    {
        let tool = job.argv.first().map(String::as_str).unwrap_or("?");
        return format!("{tool} from {}", job.from);
    }
    "Clix is running".into()
}

/// StatusNotifier when a display is set and `CLIX_TRAY` is not `0`. Missing watcher is skip.
pub(crate) fn spawn(store: Arc<Mutex<Store>>) {
    if !tray_enabled() {
        return;
    }
    tokio::spawn(async move {
        let _ = run(store).await;
    });
}

fn tray_enabled() -> bool {
    match env::var("CLIX_TRAY") {
        Ok(v) if v == "0" => false,
        _ => has_display(),
    }
}

fn has_display() -> bool {
    env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
        || env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

async fn run(store: Arc<Mutex<Store>>) {
    let tray = ClixTray {
        store: Arc::downgrade(&store),
    };
    let handle = match tray.spawn().await {
        Ok(h) => h,
        Err(_) => return,
    };
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if handle.update(|_| ()).await.is_none() {
            break;
        }
    }
}

struct ClixTray {
    store: Weak<Mutex<Store>>,
}

impl ClixTray {
    fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> Option<R> {
        let store = self.store.upgrade()?;
        let g = store.lock().unwrap_or_else(|e| e.into_inner());
        Some(f(&g))
    }

    fn with_store_mut<R>(&self, f: impl FnOnce(&mut Store) -> R) -> Option<R> {
        let store = self.store.upgrade()?;
        let mut g = store.lock().unwrap_or_else(|e| e.into_inner());
        Some(f(&mut g))
    }

    fn allow_latest(&self) {
        self.with_store_mut(|s| {
            if request::allow(s, &[], true, None, None).is_ok() {
                let _ = s.save();
            }
        });
    }
}

impl ksni::Tray for ClixTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "Clix".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn icon_name(&self) -> String {
        "computer".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![tray_icon()]
    }

    fn tool_tip(&self) -> ToolTip {
        let title = self
            .with_store(tray_tooltip)
            .unwrap_or_else(|| "Clix is running".into());
        ToolTip {
            title,
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let (pending, hands) = self
            .with_store(|s| (pending_items(s), hands_items(s)))
            .unwrap_or_else(|| (pending_items_empty(), Vec::new()));
        vec![
            SubMenu {
                label: "Pending…".into(),
                enabled: pending.iter().any(item_enabled),
                submenu: pending,
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Hands".into(),
                enabled: !hands.is_empty(),
                submenu: hands,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit daemon".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_: &mut ClixTray| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn pending_items(store: &Store) -> Vec<MenuItem<ClixTray>> {
    let mut items: Vec<MenuItem<ClixTray>> = store
        .requests
        .iter()
        .map(|r| {
            StandardItem {
                label: menu_label(&format!("{} wants {}", r.from, r.tool)),
                enabled: false,
                ..Default::default()
            }
            .into()
        })
        .collect();
    items.push(
        StandardItem {
            label: "Allow".into(),
            enabled: !store.requests.is_empty(),
            activate: Box::new(|tray: &mut ClixTray| tray.allow_latest()),
            ..Default::default()
        }
        .into(),
    );
    items
}

fn pending_items_empty() -> Vec<MenuItem<ClixTray>> {
    vec![StandardItem {
        label: "Allow".into(),
        enabled: false,
        ..Default::default()
    }
    .into()]
}

fn hands_items(store: &Store) -> Vec<MenuItem<ClixTray>> {
    store
        .grants
        .iter()
        .map(|g| {
            StandardItem {
                label: menu_label(&g.tool),
                enabled: false,
                ..Default::default()
            }
            .into()
        })
        .collect()
}

fn item_enabled(item: &MenuItem<ClixTray>) -> bool {
    matches!(item, MenuItem::Standard(StandardItem { enabled: true, .. }))
}

fn menu_label(s: &str) -> String {
    s.replace('_', "__")
}

fn tray_icon() -> ksni::Icon {
    const SIZE: i32 = 16;
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let on = (3..13).contains(&x) && (3..13).contains(&y);
            if on {
                data.extend_from_slice(&[0xff, 0x2a, 0x9d, 0x8f]);
            } else {
                data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            }
        }
    }
    ksni::Icon {
        width: SIZE,
        height: SIZE,
        data,
    }
}
