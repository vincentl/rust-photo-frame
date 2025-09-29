mod check;
mod hotspot;
mod nm;
mod ui;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::signal;
use tokio::time;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::check::Settings;
use crate::hotspot::HotspotInfo;
use crate::nm::Connectivity;

#[tokio::main]
async fn main() -> Result<()> {
    if matches!(std::env::args().nth(1).as_deref(), Some("--ui")) {
        init_tracing();
        return ui::run_from_env();
    }

    init_tracing();
    if std::env::args().any(|arg| arg == "--show-ui") {
        return run_ui_mode();
    }
    let settings = Settings::load()?;
    info!(
        wifi_ifname = %settings.wifi_ifname,
        frame_user = %settings.frame_user,
        "starting wifi watcher"
    );

    if let Err(err) = nm::ensure_wifi_radio_on() {
        warn!(?err, "failed to ensure Wi-Fi radio is on");
    }

    let mut controller = Controller::new(settings);
    controller.run().await
}

fn run_ui_mode() -> Result<()> {
    let payload =
        std::env::var("WIFI_UI_PAYLOAD").context("WIFI_UI_PAYLOAD missing for --show-ui mode")?;
    let info: HotspotInfo = serde_json::from_str(&payload).context("parsing WIFI_UI_PAYLOAD")?;
    ui::run_blocking(info)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

struct Controller {
    settings: Settings,
    ui: Option<ui::UiHandle>,
    hotspot: Option<HotspotInfo>,
}

impl Controller {
    fn new(settings: Settings) -> Self {
        Self {
            settings,
            ui: None,
            hotspot: None,
        }
    }

    async fn run(&mut self) -> Result<()> {
        let mut poll = time::interval(self.settings.poll_interval);
        poll.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        self.initial_probe().await?;

        loop {
            tokio::select! {
                _ = poll.tick() => {
                    if let Err(err) = self.check_once().await {
                        warn!(?err, "wifi watcher iteration failed");
                    }
                }
                _ = signal::ctrl_c() => {
                    info!("received shutdown signal");
                    self.cleanup();
                    break;
                }
            }
        }

        Ok(())
    }

    async fn initial_probe(&mut self) -> Result<()> {
        let deadline = Instant::now() + self.settings.startup_timeout;
        loop {
            match nm::connectivity() {
                Ok(Connectivity::Full) => {
                    info!("wifi online at startup");
                    self.on_wifi_up().await?;
                    return Ok(());
                }
                Ok(status) => {
                    info!(?status, "wifi not ready at boot");
                }
                Err(err) => warn!(?err, "failed to probe wifi status"),
            }

            if Instant::now() > deadline {
                warn!("startup connectivity check timed out");
                self.on_wifi_down().await?;
                return Ok(());
            }

            time::sleep(Duration::from_secs(3)).await;
        }
    }

    async fn check_once(&mut self) -> Result<()> {
        match nm::connectivity() {
            Ok(Connectivity::Full) => self.on_wifi_up().await?,
            Ok(other) => {
                info!(?other, "connectivity degraded");
                self.on_wifi_down().await?;
            }
            Err(err) => {
                warn!(?err, "failed to query connectivity");
                self.on_wifi_down().await?;
            }
        }
        Ok(())
    }

    async fn on_wifi_up(&mut self) -> Result<()> {
        self.ensure_photo_app_started()?;
        self.stop_hotspot()?;
        self.stop_setter()?;
        self.stop_ui();
        self.touch_wifi_flag()?;
        Ok(())
    }

    async fn on_wifi_down(&mut self) -> Result<()> {
        if let Err(err) = self.clear_wifi_flag() {
            warn!(?err, "failed to clear wifi flag");
        }
        self.stop_photo_app()?;
        let hotspot = self.ensure_hotspot()?;
        self.ensure_setter()?;
        self.ensure_ui(hotspot)?;
        Ok(())
    }

    fn ensure_hotspot(&mut self) -> Result<HotspotInfo> {
        if let Some(info) = &self.hotspot {
            return Ok(info.clone());
        }
        match hotspot::ensure_hotspot_running(&self.settings) {
            Ok(info) => {
                self.hotspot = Some(info.clone());
                Ok(info)
            }
            Err(err) => {
                warn!(?err, "failed to start hotspot");
                Err(err)
            }
        }
    }

    fn stop_hotspot_if_running(&mut self) {
        if let Err(err) = hotspot::stop_hotspot(&self.settings) {
            warn!(?err, "failed to stop hotspot");
        }
        self.hotspot = None;
    }

    fn ensure_ui(&mut self, info: HotspotInfo) -> Result<()> {
        if let Some(handle) = self.ui.as_mut() {
            if handle.is_alive()? {
                return Ok(());
            }
            self.ui = None;
        }
        match ui::spawn(&self.settings, info) {
            Ok(handle) => {
                self.ui = Some(handle);
                Ok(())
            }
            Err(err) => {
                warn!(?err, "failed to launch wifi UI");
                Err(err)
            }
        }
    }

    fn stop_ui(&mut self) {
        if let Some(mut handle) = self.ui.take() {
            handle.stop();
        }
    }

    fn touch_wifi_flag(&self) -> Result<()> {
        let path = Path::new("/run/photo-frame/wifi_up");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("creating wifi flag directory")?;
        }
        fs::write(path, b"online").context("writing wifi flag")?;
        Ok(())
    }

    fn clear_wifi_flag(&self) -> Result<()> {
        let path = Path::new("/run/photo-frame/wifi_up");
        if path.exists() {
            fs::remove_file(path).context("removing wifi flag")?;
        }
        Ok(())
    }

    fn ensure_photo_app_started(&self) -> Result<()> {
        let unit = &self.settings.photo_service_unit;
        let output = std::process::Command::new("systemctl")
            .args(["start", unit])
            .output()
            .with_context(|| format!("starting {unit}"))?;
        if !output.status.success() {
            warn!(
                "{} start returned non-zero: {}",
                unit,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn stop_photo_app(&self) -> Result<()> {
        let unit = &self.settings.photo_service_unit;
        let output = std::process::Command::new("systemctl")
            .args(["stop", unit])
            .output()
            .with_context(|| format!("stopping {unit}"))?;
        if !output.status.success() {
            warn!(
                "{} stop returned non-zero: {}",
                unit,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn stop_hotspot(&mut self) -> Result<()> {
        self.stop_hotspot_if_running();
        Ok(())
    }

    fn ensure_setter(&self) -> Result<()> {
        let unit = &self.settings.wifi_setter_unit;
        let output = std::process::Command::new("systemctl")
            .args(["start", unit])
            .output()
            .with_context(|| format!("starting {unit}"))?;
        if !output.status.success() {
            warn!(
                "{} start returned non-zero: {}",
                unit,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn stop_setter(&self) -> Result<()> {
        let unit = &self.settings.wifi_setter_unit;
        let output = std::process::Command::new("systemctl")
            .args(["stop", unit])
            .output()
            .with_context(|| format!("stopping {unit}"))?;
        if !output.status.success() {
            warn!(
                "{} stop returned non-zero: {}",
                unit,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn cleanup(&mut self) {
        self.stop_ui();
        if let Err(err) = self.stop_setter() {
            warn!(?err, "failed to stop wifi setter during cleanup");
        }
        self.stop_hotspot_if_running();
    }
}
