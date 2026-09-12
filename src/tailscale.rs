use std::collections::HashMap;
use std::ffi::OsStr;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::error::{ClixError, Result};

const TAILSCALE_OFF: &str = "Tailscale is off. Start it, then pair.";
const DEFAULT_PORT: u16 = 7421;

/// A tailnet peer Clix can dial. Not Clix identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAddr {
    pub name: String,
    pub ipv4: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub self_name: String,
    pub self_ipv4: String,
    pub peers: Vec<PeerAddr>,
}

#[derive(Deserialize)]
struct StatusJson {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(rename = "Self")]
    self_peer: Option<PeerJson>,
    #[serde(rename = "Peer")]
    peer: Option<HashMap<String, PeerJson>>,
}

#[derive(Deserialize)]
struct PeerJson {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Option<Vec<String>>,
    #[serde(rename = "Online", default)]
    online: bool,
}

fn off() -> ClixError {
    ClixError::Io(TAILSCALE_OFF.into())
}

fn first_ipv4(ips: &[String]) -> Option<String> {
    ips.iter()
        .find(|ip| ip.parse::<Ipv4Addr>().is_ok())
        .cloned()
}

/// Parse `tailscale status --json`. Online peers only; Self is not a peer.
pub fn parse_status(json: &str) -> Result<Status> {
    let raw: StatusJson = serde_json::from_str(json)?;
    if raw.backend_state != "Running" {
        return Err(off());
    }
    let self_peer = raw.self_peer.ok_or_else(off)?;
    let self_ips = self_peer.tailscale_ips.unwrap_or_default();
    let self_ipv4 = first_ipv4(&self_ips).ok_or_else(off)?;
    let mut peers: Vec<PeerAddr> = raw
        .peer
        .unwrap_or_default()
        .into_values()
        .filter(|p| p.online)
        .filter_map(|p| {
            let ipv4 = first_ipv4(&p.tailscale_ips.unwrap_or_default())?;
            if ipv4 == self_ipv4 {
                return None;
            }
            Some(PeerAddr {
                name: p.host_name,
                ipv4,
            })
        })
        .collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Status {
        self_name: self_peer.host_name,
        self_ipv4,
        peers,
    })
}

fn tailscale_bin() -> PathBuf {
    std::env::var_os("CLIX_TAILSCALE")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("tailscale"))
}

fn status_from(bin: impl AsRef<OsStr>) -> Result<Status> {
    let output = Command::new(bin)
        .arg("status")
        .arg("--json")
        .output()
        .map_err(|_| off())?;
    if !output.status.success() {
        return Err(off());
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| off())?;
    parse_status(&stdout)
}

/// Run `tailscale status --json` and return online peer IPv4s.
pub fn tailscale_status() -> Result<Vec<PeerAddr>> {
    tailscale_status_from(tailscale_bin())
}

pub(crate) fn tailscale_status_from(bin: impl AsRef<OsStr>) -> Result<Vec<PeerAddr>> {
    Ok(status_from(bin)?.peers)
}

/// `CLIX_PORT` or 7421.
pub fn mesh_port() -> u16 {
    std::env::var("CLIX_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&p| p != 0)
        .unwrap_or(DEFAULT_PORT)
}

/// This body's Tailscale IPv4 and mesh port. Pairing is still PAKE.
pub fn mesh_bind_addr() -> Result<String> {
    let st = status_from(tailscale_bin())?;
    Ok(format!("{}:{}", st.self_ipv4, mesh_port()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fixture_names_and_ips() {
        let json = include_str!("../tests/fixtures/tailscale-status.json");
        let st = parse_status(json).unwrap();
        assert_eq!(st.self_name, "laptop");
        assert_eq!(st.self_ipv4, "100.64.0.10");
        let peers: Vec<_> = st
            .peers
            .iter()
            .map(|p| (p.name.as_str(), p.ipv4.as_str()))
            .collect();
        assert_eq!(
            peers,
            vec![
                ("secondary-server", "100.64.0.30"),
                ("server", "100.64.0.20"),
            ]
        );
    }

    #[test]
    fn mocked_tailscale_failing_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tailscale");
        std::fs::write(&bin, b"#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        let err = tailscale_status_from(&bin).unwrap_err();
        assert_eq!(err.to_string(), "Tailscale is off. Start it, then pair.");
    }
}
