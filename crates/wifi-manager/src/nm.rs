use crate::config::{Config, HotspotConfig};
use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use std::collections::HashSet;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info};

#[derive(Debug, Subcommand)]
pub enum NmCommand {
    /// Ensure the hotspot profile exists and is configured for AP mode.
    EnsureHotspot,
    /// Bring the pf-hotspot connection up.
    HotspotUp,
    /// Bring the pf-hotspot connection down.
    HotspotDown,
    /// Add or update a Wi-Fi connection with the provided SSID/PSK.
    Add(AddArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub ssid: String,
    #[arg(long)]
    pub psk: String,
}

pub async fn handle_cli(cmd: NmCommand, config: &Config) -> Result<()> {
    match cmd {
        NmCommand::EnsureHotspot => {
            ensure_hotspot_profile(&config.hotspot, &config.interface, None).await?
        }
        NmCommand::HotspotUp => bring_hotspot_up(&config.hotspot).await?,
        NmCommand::HotspotDown => bring_hotspot_down(&config.hotspot).await?,
        NmCommand::Add(args) => {
            add_or_update_wifi(&config.interface, &args.ssid, &args.psk).await?;
        }
    }
    Ok(())
}

pub async fn device_connected(interface: &str) -> Result<bool> {
    let output = nmcli(&["-t", "-f", "DEVICE,STATE", "device", "status"]).await?;
    for line in output.lines() {
        let mut parts = line.split(':');
        if let (Some(dev), Some(state)) = (parts.next(), parts.next()) {
            if dev == interface {
                return Ok(state == "connected" || state == "activated" || state == "full");
            }
        }
    }
    Ok(false)
}

pub async fn gateway_reachable(interface: &str) -> Result<bool> {
    let gw = default_gateway(interface).await?;
    if let Some(gw) = gw {
        let status = Command::new("ping")
            .arg("-c")
            .arg("1")
            .arg("-W")
            .arg("2")
            .arg(&gw)
            .status()
            .await
            .with_context(|| format!("failed to spawn ping for gateway {gw}"))?;
        return Ok(status.success());
    }
    Ok(false)
}

async fn default_gateway(interface: &str) -> Result<Option<String>> {
    let output = nmcli(&["-t", "-f", "IP4.GATEWAY", "device", "show", interface]).await?;
    for line in output.lines() {
        if !line.trim().is_empty() {
            return Ok(Some(line.trim().to_string()));
        }
    }
    Ok(None)
}

pub async fn ensure_hotspot_profile(
    hotspot: &HotspotConfig,
    interface: &str,
    password: Option<&str>,
) -> Result<()> {
    let existing = list_connection_names().await?;
    if existing.contains(&hotspot.connection_id) {
        debug!(id = %hotspot.connection_id, "hotspot profile already exists; ensuring settings");
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "connection.autoconnect",
            "no",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "802-11-wireless.mode",
            "ap",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "802-11-wireless.band",
            "bg",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "802-11-wireless.ssid",
            &hotspot.ssid,
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "ipv4.method",
            "shared",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "ipv6.method",
            "ignore",
        ])
        .await?;
        if let Some(pass) = password {
            nmcli(&[
                "connection",
                "modify",
                &hotspot.connection_id,
                "wifi-sec.key-mgmt",
                "wpa-psk",
            ])
            .await?;
            nmcli(&[
                "connection",
                "modify",
                &hotspot.connection_id,
                "wifi-sec.psk",
                pass,
            ])
            .await?;
        }
    } else {
        info!(id = %hotspot.connection_id, "creating hotspot profile");
        let args = vec![
            "connection",
            "add",
            "type",
            "wifi",
            "ifname",
            interface,
            "con-name",
            &hotspot.connection_id,
            "autoconnect",
            "no",
            "ssid",
            &hotspot.ssid,
        ];
        nmcli(&args).await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "802-11-wireless.mode",
            "ap",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "802-11-wireless.band",
            "bg",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "ipv4.method",
            "shared",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "ipv6.method",
            "ignore",
        ])
        .await?;
        if let Some(pass) = password {
            nmcli(&[
                "connection",
                "modify",
                &hotspot.connection_id,
                "wifi-sec.key-mgmt",
                "wpa-psk",
            ])
            .await?;
            nmcli(&[
                "connection",
                "modify",
                &hotspot.connection_id,
                "wifi-sec.psk",
                pass,
            ])
            .await?;
        }
    }
    if let Some(pass) = password {
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "wifi-sec.key-mgmt",
            "wpa-psk",
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &hotspot.connection_id,
            "wifi-sec.psk",
            pass,
        ])
        .await?;
    }
    Ok(())
}

pub async fn bring_hotspot_up(hotspot: &HotspotConfig) -> Result<()> {
    nmcli(&["connection", "up", &hotspot.connection_id]).await?;
    Ok(())
}

pub async fn bring_hotspot_down(hotspot: &HotspotConfig) -> Result<()> {
    let res = nmcli(&["connection", "down", &hotspot.connection_id]).await;
    match res {
        Ok(_) => Ok(()),
        Err(err) => {
            debug!(error = ?err, "failed to bring hotspot down (continuing)");
            Ok(())
        }
    }
}

pub async fn add_or_update_wifi(interface: &str, ssid: &str, psk: &str) -> Result<String> {
    let connection_id = format!("pf-wifi-{}", sanitize_id(ssid));
    ensure_psk_rules(psk)?;
    let existing = list_connection_names().await?;
    if existing.contains(&connection_id) {
        info!(connection = %connection_id, "updating stored credentials");
        nmcli(&[
            "connection",
            "modify",
            &connection_id,
            "802-11-wireless.ssid",
            ssid,
        ])
        .await?;
        nmcli(&[
            "connection",
            "modify",
            &connection_id,
            "wifi-sec.key-mgmt",
            "wpa-psk",
        ])
        .await?;
        nmcli(&["connection", "modify", &connection_id, "wifi-sec.psk", psk]).await?;
        nmcli(&[
            "connection",
            "modify",
            &connection_id,
            "connection.autoconnect",
            "yes",
        ])
        .await?;
    } else {
        info!(connection = %connection_id, "adding new Wi-Fi connection profile");
        nmcli(&[
            "connection",
            "add",
            "type",
            "wifi",
            "ifname",
            interface,
            "con-name",
            &connection_id,
            "ssid",
            ssid,
            "wifi-sec.key-mgmt",
            "wpa-psk",
            "wifi-sec.psk",
            psk,
            "connection.autoconnect",
            "yes",
        ])
        .await?;
    }
    Ok(connection_id)
}

pub async fn activate_connection(connection_id: &str) -> Result<()> {
    nmcli(&["connection", "up", connection_id]).await?;
    Ok(())
}

async fn list_connection_names() -> Result<HashSet<String>> {
    let output = nmcli(&["-t", "-f", "NAME", "connection", "show"]).await?;
    Ok(output
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

fn sanitize_id(ssid: &str) -> String {
    let mut id = ssid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    if id.is_empty() {
        id.push('x');
    }
    if id.len() > 20 {
        id.truncate(20);
    }
    id.to_lowercase()
}

fn ensure_psk_rules(psk: &str) -> Result<()> {
    let len = psk.chars().count();
    if (8..=63).contains(&len) {
        Ok(())
    } else {
        Err(anyhow!("Password must be between 8 and 63 characters"))
    }
}

async fn nmcli(args: &[&str]) -> Result<String> {
    debug!(command = %display_args(args), "running nmcli");
    let mut cmd = Command::new("nmcli");
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd.output().await.context("failed to execute nmcli")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!(
            "nmcli {} failed: {}",
            display_args(args),
            stderr.trim()
        ))
    }
}

fn display_args(args: &[&str]) -> String {
    let mut masked = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args.iter() {
        if skip_next {
            masked.push("<redacted>");
            skip_next = false;
            continue;
        }
        if matches!(arg.as_ref(), "wifi-sec.psk" | "psk") {
            masked.push(arg);
            skip_next = true;
        } else {
            masked.push(arg);
        }
    }
    masked.join(" ")
}
