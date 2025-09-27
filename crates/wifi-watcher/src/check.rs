use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_yaml;

#[derive(Debug, Clone)]
pub struct Settings {
    pub wifi_ifname: String,
    pub frame_user: String,
    pub hotspot_ssid: String,
    pub hotspot_ip: String,
    pub hotspot_env_path: PathBuf,
    pub wordlist_path: PathBuf,
    pub poll_interval: Duration,
    pub startup_timeout: Duration,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct FrameConfig {
    #[serde(default)]
    wifi_ifname: Option<String>,
    #[serde(default)]
    hotspot_ssid: Option<String>,
    #[serde(default)]
    hotspot_ip: Option<String>,
}

impl Settings {
    pub fn load() -> Result<Self> {
        let config_path = PathBuf::from(
            env::var("PHOTO_FRAME_CONFIG")
                .unwrap_or_else(|_| "/etc/photo-frame/config.yaml".to_string()),
        );
        let mut wifi_ifname = env::var("WIFI_IFNAME").unwrap_or_else(|_| "wlan0".to_string());
        let mut hotspot_ssid =
            env::var("HOTSPOT_SSID").unwrap_or_else(|_| "Frame-Setup".to_string());
        let mut hotspot_ip = env::var("HOTSPOT_IP").unwrap_or_else(|_| "192.168.4.1".to_string());

        if config_path.exists() {
            if let Some(config) = read_config(&config_path).ok().flatten() {
                if let Some(value) = config.wifi_ifname {
                    wifi_ifname = value;
                }
                if let Some(value) = config.hotspot_ssid {
                    hotspot_ssid = value;
                }
                if let Some(value) = config.hotspot_ip {
                    hotspot_ip = value;
                }
            }
        }

        let frame_user = env::var("FRAME_USER").unwrap_or_else(|_| "frame".to_string());
        let poll_interval = env::var("WIFI_WATCHER_POLL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(30));
        let startup_timeout = env::var("WIFI_WATCHER_STARTUP_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(25));

        Ok(Self {
            wifi_ifname,
            frame_user,
            hotspot_ssid,
            hotspot_ip,
            hotspot_env_path: PathBuf::from("/run/photo-frame/hotspot.env"),
            wordlist_path: PathBuf::from("/opt/photo-frame/share/wordlist_small.txt"),
            poll_interval,
            startup_timeout,
        })
    }
}

fn read_config(path: &Path) -> Result<Option<FrameConfig>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading config at {}", path.display()))?;
    let cfg: FrameConfig = serde_yaml::from_str(&contents).context("parsing config.yaml")?;
    Ok(Some(cfg))
}
