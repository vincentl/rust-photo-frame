mod nm;
mod web;

use std::env;
use std::net::SocketAddr;

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
        let config_path = std::path::PathBuf::from(
            env::var("PHOTO_FRAME_CONFIG")
                .unwrap_or_else(|_| "/etc/photo-frame/config.yaml".to_string()),
        );
        let mut wifi_ifname = env::var("WIFI_IFNAME").unwrap_or_else(|_| "wlan0".to_string());
        let mut hotspot_ip = env::var("HOTSPOT_IP").unwrap_or_else(|_| "192.168.4.1".to_string());

        if config_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&config_path) {
                if let Ok(cfg) = serde_yaml::from_str::<FrameConfig>(&contents) {
                    if let Some(value) = cfg.wifi_ifname {
                        wifi_ifname = value;
                    }
                    if let Some(value) = cfg.hotspot_ip {
                        hotspot_ip = value;
                    }
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
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let settings = Settings::load()?;
    info!(?settings.bind_addr, "starting wifi setter");

    let state = AppState {
        ifname: settings.wifi_ifname.clone(),
        hotspot_ip: settings.hotspot_ip.clone(),
    };

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
