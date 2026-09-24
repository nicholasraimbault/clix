mod cli;
mod daemon;
mod error;
mod exec;
mod grant;
pub mod history;
mod install;
mod job;
mod limits;
mod local;
mod mesh;
mod notify;
mod pair;
mod paths;
mod pin;
mod pin_owner;
mod request;
mod storage;
#[cfg(test)]
mod storage_tests;
mod store;
mod tailscale;
mod transport;
mod tray;
mod types;

pub use cli::{parse_argv, Cmd, JobCommand, PinCommand, RecoveryCommand, StorageCommand};
pub use daemon::{dispatch, serve, serve_local};
pub use error::ClixError;
pub use exec::run_granted;
pub use grant::{
    add, add_with_args, check, check_args, check_at, describe as describe_grant, hands, remove,
    resolve_tool,
};
pub use local::client_send;
pub use mesh::{call as mesh_call, MeshHandle, MeshListener};
pub use notify::{display_present_in, set_notify_hook, NotifyHook};
pub use pair::{pair_join, pair_listen, phrase, sanitize_name as sanitize_body_name};
pub use paths::{socket_path, state_dir, state_file};
pub use pin::{
    inspect_peer_conflicts, owner_pin_sync, recovery_discard, recovery_export, recovery_inspect,
    recovery_list, recovery_list_page, recovery_read, recovery_resolve, take_peer_conflict,
    PinConflictReport, PinConflictSnapshot, PinSyncStatus, RecoveryChoice, RecoveryChunk,
    RecoveryContext, RecoveryExport, RecoveryInspection, RecoveryList, RecoveryRead,
    RecoveryReceipt, RecoveryState, RecoveryUsage, RecoveryVersion, MAX_RECOVERY_BYTES,
    MAX_RECOVERY_ENTRIES, MAX_RECOVERY_PAGE,
};
pub use pin::{sync as pin_sync, sync_after_pair};
pub use store::Store;
pub use tailscale::{tailscale_status, PeerAddr};
pub use tray::tray_tooltip;
pub use types::{BodyId, Grant, Job, JobStatus, OwnerId, Peer, Request, Schedule};
