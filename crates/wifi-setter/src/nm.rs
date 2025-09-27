#![allow(dead_code)]

use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    Full,
    Portal,
    Limited,
    None,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct WifiConn {
    pub name: String,
    pub uuid: String,
    pub ssid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScannedNetwork {
    pub ssid: String,
    pub signal: Option<u8>,
}

#[derive(Debug, Error)]
pub enum NmError {
    #[error("nmcli failed: {command}: {message}")]
    Command { command: String, message: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

struct CommandResult {
    stdout: String,
    stderr: String,
}

fn run_nmcli(args: &[&str], redacted: Option<&[usize]>) -> Result<CommandResult, NmError> {
    let mut display_args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    if let Some(indices) = redacted {
        for &i in indices {
            if i < display_args.len() {
                display_args[i] = "<redacted>".into();
            }
        }
    }
    debug!(?display_args, "nmcli");
    let output = Command::new("nmcli")
        .args(args)
        .output()
        .with_context(|| format!("spawning nmcli with args {display_args:?}"))?;
    if output.status.success() {
        Ok(CommandResult {
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(NmError::Command {
            command: format!("nmcli {}", display_args.join(" ")),
            message: if stderr.is_empty() { stdout } else { stderr },
        })
    }
}

pub fn connectivity() -> Result<Connectivity, NmError> {
    let result = run_nmcli(&["-t", "-f", "CONNECTIVITY", "general", "status"], None)?;
    let status = result.stdout.trim();
    Ok(match status {
        "full" => Connectivity::Full,
        "portal" => Connectivity::Portal,
        "limited" => Connectivity::Limited,
        "none" => Connectivity::None,
        _ => Connectivity::Unknown,
    })
}

pub fn list_wifi_connections() -> Result<Vec<WifiConn>, NmError> {
    let result = run_nmcli(&["-t", "-f", "NAME,TYPE,UUID", "connection", "show"], None)?;
    let mut items = Vec::new();
    for line in result.stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ':');
        let name = parts.next().unwrap_or_default().to_string();
        let ty = parts.next().unwrap_or_default();
        let uuid = parts.next().unwrap_or_default().to_string();
        if ty != "802-11-wireless" && ty != "wifi" {
            continue;
        }
        let ssid = connection_ssid(&name).ok().flatten();
        items.push(WifiConn { name, uuid, ssid });
    }
    Ok(items)
}

fn connection_ssid(name: &str) -> Result<Option<String>, NmError> {
    let result = run_nmcli(
        &[
            "-t",
            "-f",
            "802-11-wireless.ssid",
            "connection",
            "show",
            name,
        ],
        None,
    )?;
    let value = result.stdout.trim();
    if value.is_empty() || value == "--" {
        Ok(None)
    } else {
        Ok(Some(value.to_string()))
    }
}

pub fn connection_for_ssid(ssid: &str) -> Result<Option<WifiConn>, NmError> {
    let items = list_wifi_connections()?;
    Ok(items.into_iter().find(|c| c.ssid.as_deref() == Some(ssid)))
}

pub fn modify_known_wifi(name: &str, psk: &str) -> Result<(), NmError> {
    run_nmcli(
        &[
            "connection",
            "modify",
            name,
            "wifi-sec.key-mgmt",
            "wpa-psk",
            "wifi-sec.psk",
            psk,
        ],
        Some(&[6]),
    )?;
    run_nmcli(&["connection", "up", name], None)?;
    Ok(())
}

pub fn create_new_wifi(ssid: &str, psk: &str, ifname: &str) -> Result<(), NmError> {
    run_nmcli(
        &[
            "connection",
            "add",
            "type",
            "wifi",
            "ifname",
            ifname,
            "con-name",
            ssid,
            "ssid",
            ssid,
        ],
        None,
    )?;
    run_nmcli(
        &[
            "connection",
            "modify",
            ssid,
            "wifi-sec.key-mgmt",
            "wpa-psk",
            "wifi-sec.psk",
            psk,
        ],
        Some(&[6]),
    )?;
    run_nmcli(&["connection", "up", ssid], None)?;
    Ok(())
}

pub fn scan_networks() -> Result<Vec<ScannedNetwork>, NmError> {
    let result = run_nmcli(&["-t", "-f", "SSID,SIGNAL", "device", "wifi", "list"], None)?;
    let mut networks = Vec::new();
    for line in result.stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, ':');
        let ssid = parts.next().unwrap_or_default().trim();
        let signal = parts.next().and_then(|s| s.parse::<u8>().ok());
        if ssid.is_empty() {
            continue;
        }
        if networks.iter().any(|n: &ScannedNetwork| n.ssid == ssid) {
            continue;
        }
        networks.push(ScannedNetwork {
            ssid: ssid.to_string(),
            signal,
        });
    }
    networks.sort_by(|a, b| b.signal.cmp(&a.signal));
    Ok(networks)
}

pub fn active_ssid() -> Result<Option<String>, NmError> {
    let result = run_nmcli(
        &["-t", "-f", "NAME,TYPE", "connection", "show", "--active"],
        None,
    )?;
    for line in result.stdout.lines() {
        let mut parts = line.splitn(2, ':');
        let name = parts.next().unwrap_or_default();
        let ty = parts.next().unwrap_or_default();
        if ty != "802-11-wireless" && ty != "wifi" {
            continue;
        }
        if let Some(ssid) = connection_ssid(name)? {
            return Ok(Some(ssid));
        }
        return Ok(Some(name.to_string()));
    }
    Ok(None)
}
