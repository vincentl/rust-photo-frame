use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(default = "default_interface")]
    pub interface: String,
    #[serde(default = "default_check_interval")]
    pub check_interval_sec: u64,
    #[serde(default = "default_offline_grace")]
    pub offline_grace_sec: u64,
    #[serde(default = "default_wordlist_path")]
    pub wordlist_path: PathBuf,
    #[serde(default = "default_var_dir")]
    pub var_dir: PathBuf,
    #[serde(default)]
    pub hotspot: HotspotConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub overlay: OverlayConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HotspotConfig {
    #[serde(default = "default_hotspot_connection_id")]
    pub connection_id: String,
    #[serde(default = "default_hotspot_ssid")]
    pub ssid: String,
    #[serde(default = "default_hotspot_ip")]
    pub ipv4_addr: Ipv4Addr,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UiConfig {
    #[serde(default = "default_ui_bind")]
    pub bind_address: String,
    #[serde(default = "default_ui_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OverlayConfig {
    #[serde(default = "default_overlay_command")]
    pub command: Vec<String>,
    #[serde(default = "default_photo_app_id")]
    pub photo_app_id: String,
    #[serde(default = "default_overlay_app_id")]
    pub overlay_app_id: String,
    #[serde(default)]
    pub sway_socket: Option<PathBuf>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let cfg: Config = serde_yaml::from_slice(&data)
            .with_context(|| format!("failed to parse config at {}", path.display()))?;
        Ok(cfg)
    }
}

impl Default for HotspotConfig {
    fn default() -> Self {
        Self {
            connection_id: default_hotspot_connection_id(),
            ssid: default_hotspot_ssid(),
            ipv4_addr: default_hotspot_ip(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            bind_address: default_ui_bind(),
            port: default_ui_port(),
        }
    }
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            command: default_overlay_command(),
            photo_app_id: default_photo_app_id(),
            overlay_app_id: default_overlay_app_id(),
            sway_socket: None,
        }
    }
}

fn default_interface() -> String {
    "wlan0".to_string()
}

fn default_check_interval() -> u64 {
    5
}

fn default_offline_grace() -> u64 {
    30
}

fn default_wordlist_path() -> PathBuf {
    PathBuf::from("/opt/photo-frame/share/wordlist.txt")
}

fn default_var_dir() -> PathBuf {
    PathBuf::from("/var/lib/photo-frame")
}

fn default_hotspot_connection_id() -> String {
    "pf-hotspot".to_string()
}

fn default_hotspot_ssid() -> String {
    "PhotoFrame-Setup".to_string()
}

fn default_hotspot_ip() -> Ipv4Addr {
    Ipv4Addr::new(192, 168, 4, 1)
}

fn default_ui_bind() -> String {
    "0.0.0.0".to_string()
}

fn default_ui_port() -> u16 {
    8080
}

fn default_overlay_command() -> Vec<String> {
    // Launch overlay via sway so it inherits the session Wayland environment.
    // The watcher will construct a single exec command line with arguments.
    vec![
        "swaymsg".to_string(),
    ]
}

fn default_photo_app_id() -> String {
    "rust-photo-frame".to_string()
}

fn default_overlay_app_id() -> String {
    "wifi-overlay".to_string()
}
