#![allow(dead_code)]

use std::process::Command;

use anyhow::{Context, Result};
use thiserror::Error;

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

#[derive(Debug, Error)]
pub enum NmError {
    #[error("nmcli command failed: {command}: {message}")]
    CommandError { command: String, message: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

struct CommandResult {
    stdout: String,
    stderr: String,
}

fn run_nmcli(args: &[&str], redacted: Option<&[usize]>) -> Result<CommandResult, NmError> {
    let mut display_args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    if let Some(idxs) = redacted {
        for &idx in idxs {
            if idx < display_args.len() {
                display_args[idx] = "<redacted>".to_string();
            }
        }
    }
    tracing::debug!(command = ?display_args, "running nmcli");

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
        Err(NmError::CommandError {
            command: format!("nmcli {}", display_args.join(" ")),
            message: if stderr.is_empty() { stdout } else { stderr },
        })
    }
}

pub fn connectivity() -> Result<Connectivity, NmError> {
    let result = run_nmcli(&["-t", "-f", "CONNECTIVITY", "general", "status"], None)?;
    let status = result.stdout.trim();
    let connectivity = match status {
        "full" => Connectivity::Full,
        "portal" => Connectivity::Portal,
        "limited" => Connectivity::Limited,
        "none" => Connectivity::None,
        _ => Connectivity::Unknown,
    };
    Ok(connectivity)
}

pub fn list_wifi_connections() -> Result<Vec<WifiConn>, NmError> {
    let result = run_nmcli(&["-t", "-f", "NAME,TYPE,UUID", "connection", "show"], None)?;
    let mut connections = Vec::new();
    for line in result.stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ':');
        let name = parts.next().unwrap_or_default().to_string();
        let conn_type = parts.next().unwrap_or_default();
        let uuid = parts.next().unwrap_or_default().to_string();
        if conn_type != "802-11-wireless" && conn_type != "wifi" {
            continue;
        }
        let ssid = connection_ssid(&name).ok().flatten();
        connections.push(WifiConn { name, uuid, ssid });
    }
    Ok(connections)
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
    let value = result.stdout.trim().to_string();
    if value.is_empty() || value == "--" {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

pub fn connection_for_ssid(target: &str) -> Result<Option<WifiConn>, NmError> {
    let connections = list_wifi_connections()?;
    Ok(connections
        .into_iter()
        .find(|conn| conn.ssid.as_deref() == Some(target)))
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

pub fn ensure_wifi_radio_on() -> Result<(), NmError> {
    run_nmcli(&["radio", "wifi", "on"], None)?;
    Ok(())
}

pub fn disconnect_current(ifname: &str) -> Result<(), NmError> {
    run_nmcli(&["device", "disconnect", ifname], None)?;
    Ok(())
}

pub fn active_ssid() -> Result<Option<String>, NmError> {
    let result = run_nmcli(
        &["-t", "-f", "NAME,TYPE", "connection", "show", "--active"],
        None,
    )?;
    for line in result.stdout.lines() {
        let mut parts = line.splitn(2, ':');
        let name = parts.next().unwrap_or_default();
        let conn_type = parts.next().unwrap_or_default();
        if conn_type != "802-11-wireless" && conn_type != "wifi" {
            continue;
        }
        if let Some(ssid) = connection_ssid(name)? {
            return Ok(Some(ssid));
        }
        return Ok(Some(name.to_string()));
    }
    Ok(None)
}
