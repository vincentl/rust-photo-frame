use crate::config::Config;
use crate::hotspot;
use crate::nm;
use crate::qr;
use anyhow::{Context, Result};
use rand::Rng;
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

pub async fn run(config: Config, config_path: PathBuf) -> Result<()> {
    fs::create_dir_all(&config.var_dir)
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;

    let mut state = WatchState::Online;
    let mut offline_since: Option<Instant> = None;
    let mut hotspot_state: Option<ActiveHotspot> = None;

    let mut sigterm =
        signal(SignalKind::terminate()).context("failed to register SIGTERM handler")?;
    let mut sigint =
        signal(SignalKind::interrupt()).context("failed to register SIGINT handler")?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM; shutting down");
                if let Some(mut active) = hotspot_state.take() {
                    active.stop(&config).await.ok();
                }
                return Ok(());
            }
            _ = sigint.recv() => {
                info!("received SIGINT; shutting down");
                if let Some(mut active) = hotspot_state.take() {
                    active.stop(&config).await.ok();
                }
                return Ok(());
            }
            _ = async {
                let online = match check_online(&config).await {
                    Ok(result) => result,
                    Err(err) => {
                        warn!(error = ?err, "connectivity check failed; assuming offline");
                        false
                    }
                };

                state = match (state, online) {
                    (WatchState::Online, true) => WatchState::Online,
                    (WatchState::Online, false) => {
                        info!("state transition: ONLINE -> OFFLINE");
                        offline_since = Some(Instant::now());
                        WatchState::Offline
                    }
                    (WatchState::Offline, true) => {
                        info!("state transition: OFFLINE -> ONLINE");
                        offline_since = None;
                        WatchState::Online
                    }
                    (WatchState::Offline, false) => {
                        if let Some(since) = offline_since {
                            if since.elapsed().as_secs() >= config.offline_grace_sec {
                                info!("state transition: OFFLINE -> HOTSPOT");
                                match start_hotspot(&config, &config_path).await {
                                    Ok(active) => {
                                        if let Err(err) = qr::generate(&config) {
                                            warn!(error = ?err, "failed to write QR code asset");
                                        } else {
                                            info!("Hotspot password ready; QR updated");
                                        }
                                        hotspot_state = Some(active);
                                        WatchState::Hotspot
                                    }
                                    Err(err) => {
                                        error!(error = ?err, "failed to start hotspot");
                                        WatchState::Offline
                                    }
                                }
                            } else {
                                WatchState::Offline
                            }
                        } else {
                            offline_since = Some(Instant::now());
                            WatchState::Offline
                        }
                    }
                    (WatchState::Hotspot, true) => {
                        info!("state transition: HOTSPOT -> ONLINE");
                        if let Some(mut active) = hotspot_state.take() {
                            active.stop(&config).await.ok();
                        }
                        offline_since = None;
                        WatchState::Online
                    }
                    (WatchState::Hotspot, false) => {
                        WatchState::Hotspot
                    }
                };

                let jitter_ms: u64 = rand::thread_rng().gen_range(0..500);
                let base = Duration::from_secs(config.check_interval_sec);
                sleep(base + Duration::from_millis(jitter_ms)).await;
            } => {}
        }
    }
}

async fn start_hotspot(config: &Config, config_path: &PathBuf) -> Result<ActiveHotspot> {
    let words = hotspot::activate(config).await?;
    debug!(
        word_count = words.len(),
        "hotspot session password generated"
    );
    let child = spawn_ui(config_path).await?;
    Ok(ActiveHotspot { ui_process: child })
}

async fn check_online(config: &Config) -> Result<bool> {
    let connected = nm::device_connected(&config.interface).await?;
    if !connected {
        return Ok(false);
    }
    let gateway = nm::gateway_reachable(&config.interface).await?;
    Ok(gateway)
}

struct ActiveHotspot {
    ui_process: Child,
}

impl ActiveHotspot {
    async fn stop(&mut self, config: &Config) -> Result<()> {
        hotspot::deactivate(config).await?;
        if let Some(id) = self.ui_process.id() {
            info!(pid = id, "stopping UI process");
            self.ui_process.start_kill()?;
        }
        let _ = self.ui_process.wait().await;
        Ok(())
    }
}

async fn spawn_ui(config_path: &PathBuf) -> Result<Child> {
    let exe = std::env::current_exe().context("failed to determine current executable path")?;
    let mut command = Command::new(exe);
    command.arg("ui").arg("--config").arg(config_path);
    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
    let child = command.spawn().context("failed to spawn ui process")?;
    info!(pid = child.id(), "ui server spawned");
    Ok(child)
}

#[derive(Clone, Copy)]
enum WatchState {
    Online,
    Offline,
    Hotspot,
}
