use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::error::{ClixError, Result};

const UNIT_TEMPLATE: &str = include_str!("../systemd/clix.service");
const DEFAULT_EXE: &str = "/usr/bin/clix";

/// Write `clix.service` under the systemd --user dir, then `daemon-reload` and `enable --now`.
pub fn install() -> Result<()> {
    write_unit()?;
    enable_now()?;
    if dir_overridden() {
        return Ok(());
    }
    advise_linger();
    let sock = crate::paths::socket_path()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if crate::local::client_send_timeout(
            &sock,
            serde_json::json!({"op":"status"}),
            Some(std::time::Duration::from_millis(500)),
        )
        .is_ok()
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(ClixError::Io(
        "Clix did not become ready. Inspect journalctl --user -u clix.service".into(),
    ))
}

/// `$CLIX_SYSTEMD_DIR`, else `$XDG_CONFIG_HOME/systemd/user`, else `~/.config/systemd/user`.
fn systemd_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("CLIX_SYSTEMD_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("systemd/user"));
        }
    }
    match env::var("HOME") {
        Ok(home) if !home.is_empty() => Ok(PathBuf::from(home).join(".config/systemd/user")),
        _ => Err(ClixError::Io(
            "cannot determine systemd user directory".into(),
        )),
    }
}

fn current_bin() -> Result<String> {
    let exe = env::current_exe()?;
    exe.to_str()
        .ok_or_else(|| ClixError::Io("clix path is not utf-8".into()))
        .map(str::to_string)
}

fn systemd_quote(path: &str) -> String {
    if path
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\\' | '\''))
    {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        path.to_string()
    }
}

fn unit_text() -> Result<String> {
    let exe = systemd_quote(&current_bin()?);
    Ok(UNIT_TEMPLATE.replace(DEFAULT_EXE, &exe))
}

fn write_unit() -> Result<()> {
    let dir = systemd_dir()?;
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("clix.service"), unit_text()?)?;
    Ok(())
}

fn dir_overridden() -> bool {
    env::var("CLIX_SYSTEMD_DIR").is_ok_and(|s| !s.is_empty())
}

fn enable_now() -> Result<()> {
    // Tests set CLIX_SYSTEMD_DIR; do not enable a real user unit.
    if dir_overridden() {
        return Ok(());
    }
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", "clix.service"])
}

/// A systemd --user service only survives logout, and only starts at boot,
/// when the user is lingering. On a headless server without lingering the daemon
/// stops when the SSH session ends, so advise enabling it when it is off.
fn advise_linger() {
    if linger_enabled() == Some(true) {
        return;
    }
    let user = env::var("USER").unwrap_or_default();
    let target = if user.is_empty() {
        String::new()
    } else {
        format!(" {user}")
    };
    eprintln!(
        "Note: enable lingering so Clix keeps running without an active login \
         session (needed on a headless server):\n    loginctl enable-linger{target}"
    );
}

fn linger_enabled() -> Option<bool> {
    let user = env::var("USER").ok()?;
    let out = Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger", "--value"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let bin = env::var("CLIX_SYSTEMCTL").unwrap_or_else(|_| "systemctl".into());
    let status = Command::new(&bin)
        .arg("--user")
        .args(args)
        .status()
        .map_err(|e| ClixError::Io(format!("systemctl: {e}")))?;
    if !status.success() {
        return Err(ClixError::Io(format!(
            "systemctl --user {} failed",
            args.join(" ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvRestore {
        key: &'static str,
        old: Option<OsString>,
    }

    impl EnvRestore {
        fn set(key: &'static str, val: impl AsRef<OsStr>) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, val);
            Self { key, old }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn install_writes_unit_under_clix_systemd_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _dir = EnvRestore::set("CLIX_SYSTEMD_DIR", dir.path());
        install().unwrap();
        let unit = std::fs::read_to_string(dir.path().join("clix.service")).unwrap();
        let exe = std::env::current_exe().unwrap();
        let exe = exe.to_str().unwrap();
        assert!(
            unit.contains(&format!("ExecStart={exe} daemon")),
            "unit:\n{unit}"
        );
        assert!(unit.contains("Restart=on-failure"), "unit:\n{unit}");
    }
}
