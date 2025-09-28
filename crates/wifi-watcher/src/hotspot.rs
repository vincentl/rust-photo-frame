use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use rand::seq::SliceRandom;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::check::Settings;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotspotInfo {
    pub ssid: String,
    pub password: String,
    pub ip: String,
}

pub fn ensure_hotspot_running(settings: &Settings) -> Result<HotspotInfo> {
    let unit = format!("wifi-hotspot@{}.service", settings.wifi_ifname);
    if is_active(&unit)? {
        if let Some(info) = read_env(
            &settings.hotspot_env_path,
            &settings.hotspot_ssid,
            &settings.hotspot_ip,
        )? {
            return Ok(info);
        }
    }

    let password = generate_password(&settings.wordlist_path)?;
    write_env(
        &settings.hotspot_env_path,
        &settings.hotspot_ssid,
        &password,
        &settings.hotspot_ip,
    )?;
    run_systemctl(&["start", &unit])?;
    info!(unit, "hotspot started");
    Ok(HotspotInfo {
        ssid: settings.hotspot_ssid.clone(),
        password,
        ip: settings.hotspot_ip.clone(),
    })
}

pub fn stop_hotspot(settings: &Settings) -> Result<()> {
    let unit = format!("wifi-hotspot@{}.service", settings.wifi_ifname);
    if is_active(&unit)? {
        run_systemctl(&["stop", &unit])?;
        info!(unit, "hotspot stopped");
    }
    Ok(())
}

fn is_active(unit: &str) -> Result<bool> {
    let status = Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .with_context(|| format!("checking status of {unit}"))?;
    Ok(status.status.success())
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .with_context(|| format!("systemctl {:?}", args))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!("systemctl {:?} failed: {}", args, stderr.trim()))
    }
}

fn generate_password(wordlist_path: &Path) -> Result<String> {
    let words = read_wordlist(wordlist_path).unwrap_or_else(|_| fallback_wordlist());
    let mut rng = thread_rng();
    let mut selected = Vec::new();
    for _ in 0..3 {
        if let Some(word) = words.choose(&mut rng) {
            selected.push((*word).to_string());
        }
    }
    if selected.len() < 3 {
        selected.extend_from_slice(&["photo".into(), "frame".into(), "friend".into()]);
    }
    Ok(selected.join(" "))
}

fn read_wordlist(path: &Path) -> Result<Vec<String>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading hotspot wordlist at {}", path.display()))?;
    let mut words = Vec::new();
    for line in contents.lines() {
        let word = line.trim();
        if word.is_empty() || !word.chars().all(|c| c.is_ascii_lowercase()) {
            continue;
        }
        words.push(word.to_string());
    }
    if words.len() < 16 {
        anyhow::bail!("wordlist must contain at least 16 entries");
    }
    Ok(words)
}

fn fallback_wordlist() -> Vec<String> {
    vec![
        "apricot".into(),
        "breeze".into(),
        "camera".into(),
        "daisy".into(),
        "echo".into(),
        "forest".into(),
        "garden".into(),
        "harbor".into(),
        "island".into(),
        "jelly".into(),
        "lantern".into(),
        "maple".into(),
        "ocean".into(),
        "pepper".into(),
        "quartz".into(),
        "river".into(),
        "sunset".into(),
        "tulip".into(),
        "velvet".into(),
        "willow".into(),
    ]
}

fn write_env(path: &Path, ssid: &str, password: &str, ip: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut file = File::create(path)
        .with_context(|| format!("creating hotspot env at {}", path.display()))?;
    writeln!(file, "HOTSPOT_SSID=\"{}\"", ssid.replace('"', ""))?;
    writeln!(file, "HOTSPOT_PASSWORD=\"{}\"", password.replace('"', ""))?;
    writeln!(file, "HOTSPOT_IP=\"{}\"", ip.replace('"', ""))?;
    Ok(())
}

fn read_env(path: &Path, default_ssid: &str, default_ip: &str) -> Result<Option<HotspotInfo>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("reading hotspot env at {}", path.display()))
        }
    };
    let mut ssid = default_ssid.to_string();
    let mut password = None;
    let mut ip = default_ip.to_string();
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("HOTSPOT_SSID=") {
            ssid = rest.trim_matches('"').to_string();
        } else if let Some(rest) = line.strip_prefix("HOTSPOT_PASSWORD=") {
            password = Some(rest.trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("HOTSPOT_IP=") {
            ip = rest.trim_matches('"').to_string();
        }
    }
    Ok(password.map(|pw| HotspotInfo {
        ssid,
        password: pw,
        ip,
    }))
}
