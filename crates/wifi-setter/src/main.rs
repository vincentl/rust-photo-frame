mod nm;
mod web;

use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use web::AppState;

#[derive(Debug, Clone)]
struct Settings {
    wifi_ifname: String,
    hotspot_ip: String,
    bind_addr: SocketAddr,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct FrameConfig {
    #[serde(default)]
    wifi_ifname: Option<String>,
    #[serde(default)]
    hotspot_ip: Option<String>,
}

impl Settings {
    fn load() -> Result<Self> {
        let config_path = Self::config_path();
        let mut wifi_ifname = env::var("WIFI_IFNAME").unwrap_or_else(|_| "wlan0".to_string());
        let mut hotspot_ip = env::var("HOTSPOT_IP").unwrap_or_else(|_| "192.168.4.1".to_string());

        if config_path.exists() {
            if let Some(cfg) = read_config(&config_path).ok().flatten() {
                if let Some(value) = cfg.wifi_ifname {
                    wifi_ifname = value;
                }
                if let Some(value) = cfg.hotspot_ip {
                    hotspot_ip = value;
                }
            }
        }

        let bind_addr: SocketAddr = env::var("WIFI_SETTER_BIND")
            .unwrap_or_else(|_| "0.0.0.0:80".to_string())
            .parse()
            .context("parsing WIFI_SETTER_BIND")?;

        Ok(Self {
            wifi_ifname,
            hotspot_ip,
            bind_addr,
        })
    }

    fn config_path() -> PathBuf {
        env::var("PHOTO_FRAME_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/photo-frame/config.yaml"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let settings = Settings::load()?;
    info!(bind_addr = %settings.bind_addr, wifi_ifname = %settings.wifi_ifname, "starting wifi setter");

    let state = AppState::from(&settings);

    let listener = TcpListener::bind(settings.bind_addr)
        .await
        .context("binding wifi setter socket")?;
    info!("listening for wifi provisioning requests");

    axum::serve(listener, web::app(state))
        .with_graceful_shutdown(shutdown())
        .await
        .context("serving wifi setter")?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown() {
    if signal::ctrl_c().await.is_ok() {
        info!("wifi setter received shutdown signal");
    } else {
        warn!("wifi setter shutdown signal stream errored");
    }
}

fn read_config(path: &Path) -> Result<Option<FrameConfig>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading config at {}", path.display()))?;
    let cfg: FrameConfig = serde_yaml::from_str(&contents).context("parsing config.yaml")?;
    Ok(Some(cfg))
}

impl From<&Settings> for AppState {
    fn from(settings: &Settings) -> Self {
        AppState::new(settings.wifi_ifname.clone(), settings.hotspot_ip.clone())
    }
}
