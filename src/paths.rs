use std::env;
use std::path::PathBuf;

use directories::ProjectDirs;

use crate::error::{ClixError, Result};

/// State directory: `$CLIX_HOME` or `directories::ProjectDirs` (`clix` / `clix`).
pub fn state_dir() -> Result<PathBuf> {
    if let Ok(home) = env::var("CLIX_HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    let dirs = ProjectDirs::from("", "clix", "clix")
        .ok_or_else(|| ClixError::Io("cannot determine clix state directory".into()))?;
    dirs.state_dir()
        .map(PathBuf::from)
        .ok_or_else(|| ClixError::Io("cannot determine clix state directory".into()))
}

/// `$CLIX_HOME/state.json` (or the ProjectDirs state dir).
pub fn state_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("state.json"))
}

/// Unix socket: `$CLIX_SOCK`, else `$XDG_RUNTIME_DIR/clix.sock`.
pub fn socket_path() -> Result<PathBuf> {
    if let Ok(sock) = env::var("CLIX_SOCK") {
        if !sock.is_empty() {
            return Ok(PathBuf::from(sock));
        }
    }
    match env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join("clix.sock")),
        _ => Err(ClixError::Io(
            "XDG_RUNTIME_DIR is not set (or set CLIX_SOCK)".into(),
        )),
    }
}
