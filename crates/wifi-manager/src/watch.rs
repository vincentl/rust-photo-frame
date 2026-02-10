use crate::config::{Config, RecoveryMode};
use crate::hotspot;
use crate::nm;
use crate::overlay::{OverlayController, overlay_request};
use crate::qr;
use crate::status::{
    AttemptRecord, ProvisionRequest, RuntimeStateRecord, now_rfc3339, read_request, redact_ssid,
    remove_request, write_last_attempt, write_runtime_state,
};
use anyhow::{Context, Result};
use rand::Rng;
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

pub async fn run(config: Config, config_path: PathBuf) -> Result<()> {
    fs::create_dir_all(&config.var_dir)
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;

    let mut state = WatchState::Online;
    let mut offline_since: Option<Instant> = None;
    let mut backoff_until: Option<Instant> = None;
    let mut recovery: Option<ActiveRecovery> = None;
    let mut overlay = OverlayController::new(config.overlay.clone());

    if config.photo_app.app_id != config.overlay.photo_app_id {
        warn!(
            photo_app_id = %config.photo_app.app_id,
            overlay_photo_app_id = %config.overlay.photo_app_id,
            "photo-app.app-id and overlay.photo-app-id differ; prefer keeping them aligned"
        );
    }

    transition_state(&config, &mut state, WatchState::Online, "startup", None);

    let mut sigterm =
        signal(SignalKind::terminate()).context("failed to register SIGTERM handler")?;
    let mut sigint =
        signal(SignalKind::interrupt()).context("failed to register SIGINT handler")?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM; shutting down");
                shutdown_recovery(&config, &mut recovery, &mut overlay).await;
                return Ok(());
            }
            _ = sigint.recv() => {
                info!("received SIGINT; shutting down");
                shutdown_recovery(&config, &mut recovery, &mut overlay).await;
                return Ok(());
            }
            _ = async {
                let online = match check_online_link(&config).await {
                    Ok(result) => result,
                    Err(err) => {
                        warn!(error = ?err, "connectivity check failed; assuming offline");
                        false
                    }
                };

                match state {
                    WatchState::Online => {
                        if !online {
                            offline_since = Some(Instant::now());
                            transition_state(
                                &config,
                                &mut state,
                                WatchState::OfflineGrace,
                                "link-lost",
                                None,
                            );
                        }
                    }
                    WatchState::OfflineGrace => {
                        if online {
                            offline_since = None;
                            transition_state(
                                &config,
                                &mut state,
                                WatchState::Online,
                                "link-restored-before-grace",
                                None,
                            );
                        } else if let Some(since) = offline_since
                            && since.elapsed().as_secs() >= config.offline_grace_sec
                        {
                            match enter_recovery(&config, &config_path, &mut overlay).await {
                                Ok(active) => {
                                    recovery = Some(active);
                                    backoff_until = None;
                                    transition_state(
                                        &config,
                                        &mut state,
                                        WatchState::RecoveryHotspotActive,
                                        "offline-grace-expired",
                                        None,
                                    );
                                }
                                Err(err) => {
                                    error!(error = ?err, "failed to start recovery mode");
                                    backoff_until = Some(Instant::now() + Duration::from_secs(3));
                                    transition_state(
                                        &config,
                                        &mut state,
                                        WatchState::RecoveryBackoff,
                                        "recovery-start-failed",
                                        None,
                                    );
                                }
                            }
                        }
                    }
                    WatchState::RecoveryHotspotActive => {
                        if recovery.is_none() {
                            match enter_recovery(&config, &config_path, &mut overlay).await {
                                Ok(active) => {
                                    recovery = Some(active);
                                    transition_state(
                                        &config,
                                        &mut state,
                                        WatchState::RecoveryHotspotActive,
                                        "recovery-session-rebuilt",
                                        None,
                                    );
                                }
                                Err(err) => {
                                    error!(
                                        error = ?err,
                                        "failed to rebuild recovery session while in hotspot state"
                                    );
                                    backoff_until =
                                        Some(Instant::now() + Duration::from_secs(3));
                                    transition_state(
                                        &config,
                                        &mut state,
                                        WatchState::RecoveryBackoff,
                                        "recovery-rebuild-failed",
                                        None,
                                    );
                                }
                            }
                        } else if online {
                            finalize_recovery(
                                &config,
                                &mut recovery,
                                &mut overlay,
                                "link-restored",
                                None,
                            )
                            .await;
                            offline_since = None;
                            backoff_until = None;
                            transition_state(
                                &config,
                                &mut state,
                                WatchState::Online,
                                "link-restored",
                                None,
                            );
                        } else {
                            let request = match read_request(&config) {
                                Ok(value) => value,
                                Err(err) => {
                                    warn!(error = ?err, "failed to read provisioning request");
                                    None
                                }
                            };

                            if let Some(request) = request {
                                transition_state(
                                    &config,
                                    &mut state,
                                    WatchState::ProvisioningAttempt,
                                    "provision-request",
                                    Some(&request.attempt_id),
                                );
                                let outcome = apply_provision_request(
                                    &config,
                                    &request,
                                    &mut recovery,
                                    &mut overlay,
                                )
                                .await;
                                if let Err(err) = remove_request(&config) {
                                    warn!(error = ?err, "failed to clear provisioning request file");
                                }
                                match outcome {
                                    ProvisionOutcome::Connected => {
                                        finalize_recovery(
                                            &config,
                                            &mut recovery,
                                            &mut overlay,
                                            "provision-success",
                                            Some(&request.attempt_id),
                                        )
                                        .await;
                                        offline_since = None;
                                        backoff_until = None;
                                        transition_state(
                                            &config,
                                            &mut state,
                                            WatchState::Online,
                                            "provision-success",
                                            Some(&request.attempt_id),
                                        );
                                    }
                                    ProvisionOutcome::Failed => {
                                        backoff_until =
                                            Some(Instant::now() + Duration::from_secs(3));
                                        transition_state(
                                            &config,
                                            &mut state,
                                            WatchState::RecoveryBackoff,
                                            "provision-failed",
                                            Some(&request.attempt_id),
                                        );
                                    }
                                }
                            } else if should_run_reconnect_probe(&config, &recovery) {
                                let probe_success =
                                    run_reconnect_probe(&config, &mut recovery, &mut overlay).await;
                                if probe_success {
                                    finalize_recovery(
                                        &config,
                                        &mut recovery,
                                        &mut overlay,
                                        "probe-success",
                                        None,
                                    )
                                    .await;
                                    offline_since = None;
                                    backoff_until = None;
                                    transition_state(
                                        &config,
                                        &mut state,
                                        WatchState::Online,
                                        "probe-success",
                                        None,
                                    );
                                } else {
                                    backoff_until = Some(Instant::now() + Duration::from_secs(3));
                                    transition_state(
                                        &config,
                                        &mut state,
                                        WatchState::RecoveryBackoff,
                                        "probe-failed",
                                        None,
                                    );
                                }
                            }
                        }
                    }
                    WatchState::ProvisioningAttempt => {
                        transition_state(
                            &config,
                            &mut state,
                            WatchState::RecoveryHotspotActive,
                            "provisioning-idle",
                            None,
                        );
                    }
                    WatchState::RecoveryBackoff => {
                        if online {
                            finalize_recovery(
                                &config,
                                &mut recovery,
                                &mut overlay,
                                "link-restored-during-backoff",
                                None,
                            )
                            .await;
                            offline_since = None;
                            backoff_until = None;
                            transition_state(
                                &config,
                                &mut state,
                                WatchState::Online,
                                "link-restored-during-backoff",
                                None,
                            );
                        } else if backoff_until
                            .map(|deadline| Instant::now() >= deadline)
                            .unwrap_or(true)
                        {
                            if recovery.is_some() {
                                transition_state(
                                    &config,
                                    &mut state,
                                    WatchState::RecoveryHotspotActive,
                                    "backoff-expired",
                                    None,
                                );
                            } else {
                                match enter_recovery(&config, &config_path, &mut overlay).await {
                                    Ok(active) => {
                                        recovery = Some(active);
                                        transition_state(
                                            &config,
                                            &mut state,
                                            WatchState::RecoveryHotspotActive,
                                            "backoff-recovery-retry-success",
                                            None,
                                        );
                                    }
                                    Err(err) => {
                                        error!(
                                            error = ?err,
                                            "failed to start recovery session after backoff"
                                        );
                                        backoff_until =
                                            Some(Instant::now() + Duration::from_secs(3));
                                        transition_state(
                                            &config,
                                            &mut state,
                                            WatchState::RecoveryBackoff,
                                            "backoff-recovery-retry-failed",
                                            None,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                let jitter_ms: u64 = rand::rng().random_range(0..500);
                let base = Duration::from_secs(config.check_interval_sec);
                sleep(base + Duration::from_millis(jitter_ms)).await;
            } => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchState {
    Online,
    OfflineGrace,
    RecoveryHotspotActive,
    ProvisioningAttempt,
    RecoveryBackoff,
}

impl WatchState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::OfflineGrace => "OfflineGrace",
            Self::RecoveryHotspotActive => "RecoveryHotspotActive",
            Self::ProvisioningAttempt => "ProvisioningAttempt",
            Self::RecoveryBackoff => "RecoveryBackoff",
        }
    }
}

struct ActiveRecovery {
    ui_process: Child,
    last_reconnect_probe: Instant,
}

impl ActiveRecovery {
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

enum ProvisionOutcome {
    Connected,
    Failed,
}

fn transition_state(
    config: &Config,
    state: &mut WatchState,
    next: WatchState,
    reason: &str,
    attempt_id: Option<&str>,
) {
    if *state != next {
        info!(
            from = %state.as_str(),
            to = %next.as_str(),
            reason,
            attempt_id = attempt_id.unwrap_or("-"),
            "state transition"
        );
        *state = next;
    }

    let record = RuntimeStateRecord {
        timestamp: now_rfc3339().unwrap_or_else(|_| "unknown".to_string()),
        state: next.as_str().to_string(),
        reason: reason.to_string(),
        attempt_id: attempt_id.map(ToString::to_string),
    };
    if let Err(err) = write_runtime_state(config, &record) {
        warn!(error = ?err, "failed to persist runtime state");
    }
}

async fn check_online_link(config: &Config) -> Result<bool> {
    let connected =
        nm::connected_to_infrastructure(&config.interface, &config.hotspot.connection_id).await?;
    if connected {
        match nm::gateway_reachable(&config.interface).await {
            Ok(gateway) => {
                debug!(gateway_reachable = gateway, "gateway reachability sample");
            }
            Err(err) => {
                debug!(error = ?err, "gateway reachability sample failed");
            }
        }
    }
    Ok(connected)
}

fn should_run_reconnect_probe(config: &Config, recovery: &Option<ActiveRecovery>) -> bool {
    let Some(active) = recovery else {
        return false;
    };
    should_probe_at(config, active.last_reconnect_probe)
}

fn should_probe_at(config: &Config, last_probe: Instant) -> bool {
    last_probe.elapsed().as_secs() >= config.recovery_reconnect_probe_sec
}

async fn enter_recovery(
    config: &Config,
    config_path: &PathBuf,
    overlay: &mut OverlayController,
) -> Result<ActiveRecovery> {
    let words = hotspot::activate(config).await?;
    debug!(
        word_count = words.len(),
        "hotspot session password generated"
    );

    if let Err(err) = qr::generate(config) {
        warn!(error = ?err, "failed to write QR code asset");
    }

    let child = spawn_ui(config_path).await?;

    if let Err(err) = overlay.show(&overlay_request(config)).await {
        warn!(error = ?err, "failed to display hotspot overlay");
    }

    if config.recovery_mode == RecoveryMode::AppHandoff {
        if let Err(err) = overlay.kill_app(&config.photo_app.app_id).await {
            warn!(error = ?err, app_id = %config.photo_app.app_id, "failed to stop photo app during handoff");
        }
        if let Ok(false) = wait_for_app_presence(
            overlay,
            &config.photo_app.app_id,
            false,
            Duration::from_secs(3),
        )
        .await
        {
            warn!(app_id = %config.photo_app.app_id, "photo app still visible after handoff kill");
        }
    }

    Ok(ActiveRecovery {
        ui_process: child,
        last_reconnect_probe: Instant::now(),
    })
}

async fn finalize_recovery(
    config: &Config,
    recovery: &mut Option<ActiveRecovery>,
    overlay: &mut OverlayController,
    reason: &str,
    attempt_id: Option<&str>,
) {
    if let Some(mut active) = recovery.take()
        && let Err(err) = active.stop(config).await
    {
        warn!(error = ?err, "failed to stop recovery resources");
    }

    if let Err(err) = overlay.hide().await {
        warn!(error = ?err, "failed to hide recovery overlay");
    }

    if config.recovery_mode == RecoveryMode::AppHandoff {
        let app_visible = match overlay.app_present(&config.photo_app.app_id).await {
            Ok(present) => present,
            Err(err) => {
                warn!(error = ?err, app_id = %config.photo_app.app_id, "failed to check photo app presence");
                false
            }
        };
        if !app_visible {
            if let Err(err) = overlay
                .launch_app(&config.photo_app.app_id, &config.photo_app.launch_command)
                .await
            {
                warn!(error = ?err, "failed to relaunch photo app after recovery");
            } else if let Ok(false) = wait_for_app_presence(
                overlay,
                &config.photo_app.app_id,
                true,
                Duration::from_secs(5),
            )
            .await
            {
                warn!(app_id = %config.photo_app.app_id, "photo app did not appear after relaunch");
            }
        }
    }

    info!(
        reason,
        attempt_id = attempt_id.unwrap_or("-"),
        "recovery mode finalized"
    );
}

async fn shutdown_recovery(
    config: &Config,
    recovery: &mut Option<ActiveRecovery>,
    overlay: &mut OverlayController,
) {
    if let Some(mut active) = recovery.take()
        && let Err(err) = active.stop(config).await
    {
        warn!(error = ?err, "failed to stop hotspot while shutting down");
    }
    if let Err(err) = overlay.hide().await {
        warn!(error = ?err, "failed to hide overlay while shutting down");
    }
}

async fn apply_provision_request(
    config: &Config,
    request: &ProvisionRequest,
    recovery: &mut Option<ActiveRecovery>,
    overlay: &mut OverlayController,
) -> ProvisionOutcome {
    let connecting_msg = format!("Attempting connection to {}…", redact_ssid(&request.ssid));
    if let Err(err) = write_last_attempt(
        config,
        &AttemptRecord {
            timestamp: now_rfc3339().unwrap_or_else(|_| "unknown".to_string()),
            status: "connecting".to_string(),
            message: connecting_msg,
            ssid: redact_ssid(&request.ssid),
            attempt_id: Some(request.attempt_id.clone()),
            error: None,
        },
    ) {
        warn!(error = ?err, "failed to persist connecting status");
    }

    let connection_id = match nm::add_or_update_wifi(
        &config.interface,
        &request.ssid,
        &request.password,
    )
    .await
    {
        Ok(value) => value,
        Err(err) => {
            record_attempt_error(
                config,
                request,
                "Failed to save Wi-Fi credentials.",
                err.to_string(),
            );
            if let Err(recover_err) = ensure_hotspot_active(config, recovery, overlay).await {
                warn!(error = ?recover_err, "failed to recover hotspot after provisioning save failure");
            }
            return ProvisionOutcome::Failed;
        }
    };

    if let Err(err) = hotspot::deactivate(config).await {
        warn!(error = ?err, "failed to disable hotspot before applying credentials");
    }

    if let Err(err) = nm::activate_connection(&connection_id).await {
        record_attempt_error(
            config,
            request,
            "Failed to activate Wi-Fi connection.",
            err.to_string(),
        );
        if let Err(recover_err) = ensure_hotspot_active(config, recovery, overlay).await {
            warn!(error = ?recover_err, "failed to recover hotspot after activation error");
        }
        return ProvisionOutcome::Failed;
    }

    if wait_for_infrastructure_online(config, config.recovery_connect_timeout_sec).await {
        let msg = "Frame is back online.".to_string();
        if let Err(err) = write_last_attempt(
            config,
            &AttemptRecord {
                timestamp: now_rfc3339().unwrap_or_else(|_| "unknown".to_string()),
                status: "connected".to_string(),
                message: msg,
                ssid: redact_ssid(&request.ssid),
                attempt_id: Some(request.attempt_id.clone()),
                error: None,
            },
        ) {
            warn!(error = ?err, "failed to persist connected status");
        }
        ProvisionOutcome::Connected
    } else {
        record_attempt_error(
            config,
            request,
            "Unable to confirm connection. Double-check the password and try again.",
            "connection timeout".to_string(),
        );
        if let Err(err) = ensure_hotspot_active(config, recovery, overlay).await {
            warn!(error = ?err, "failed to restore hotspot after connection timeout");
        }
        ProvisionOutcome::Failed
    }
}

fn record_attempt_error(config: &Config, request: &ProvisionRequest, message: &str, error: String) {
    if let Err(err) = write_last_attempt(
        config,
        &AttemptRecord {
            timestamp: now_rfc3339().unwrap_or_else(|_| "unknown".to_string()),
            status: "error".to_string(),
            message: message.to_string(),
            ssid: redact_ssid(&request.ssid),
            attempt_id: Some(request.attempt_id.clone()),
            error: Some(error),
        },
    ) {
        warn!(error = ?err, "failed to persist error status");
    }
}

async fn ensure_hotspot_active(
    config: &Config,
    recovery: &mut Option<ActiveRecovery>,
    overlay: &mut OverlayController,
) -> Result<()> {
    if let Some(active) = recovery.as_mut() {
        active.last_reconnect_probe = Instant::now();
    }
    nm::bring_hotspot_up(&config.hotspot).await?;
    if let Err(err) = overlay.show(&overlay_request(config)).await {
        warn!(error = ?err, "failed to restore overlay while bringing hotspot back");
    }
    Ok(())
}

async fn run_reconnect_probe(
    config: &Config,
    recovery: &mut Option<ActiveRecovery>,
    overlay: &mut OverlayController,
) -> bool {
    info!("running reconnect probe while hotspot is active");
    if let Some(active) = recovery.as_mut() {
        active.last_reconnect_probe = Instant::now();
    }

    if let Err(err) = hotspot::deactivate(config).await {
        warn!(error = ?err, "failed to down hotspot for reconnect probe");
    }

    let connected =
        wait_for_infrastructure_online(config, config.recovery_connect_timeout_sec).await;
    if connected {
        return true;
    }

    if let Err(err) = ensure_hotspot_active(config, recovery, overlay).await {
        warn!(error = ?err, "failed to restore hotspot after reconnect probe");
    }
    false
}

async fn wait_for_infrastructure_online(config: &Config, timeout_sec: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    loop {
        match check_online_link(config).await {
            Ok(true) => return true,
            Ok(false) => {}
            Err(err) => warn!(error = ?err, "connectivity check failed while waiting for link"),
        }

        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_app_presence(
    overlay: &OverlayController,
    app_id: &str,
    expected: bool,
    timeout: Duration,
) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let present = overlay.app_present(app_id).await?;
        if present == expected {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(Duration::from_millis(200)).await;
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

#[cfg(test)]
mod tests {
    use super::{WatchState, should_probe_at};
    use crate::config::Config;
    use std::time::{Duration, Instant};

    #[test]
    fn state_labels_are_stable() {
        assert_eq!(WatchState::Online.as_str(), "Online");
        assert_eq!(WatchState::OfflineGrace.as_str(), "OfflineGrace");
        assert_eq!(
            WatchState::RecoveryHotspotActive.as_str(),
            "RecoveryHotspotActive"
        );
    }

    #[test]
    fn reconnect_probe_requires_elapsed_interval() {
        let cfg: Config = serde_yaml::from_str("{}").expect("config");
        assert!(!should_probe_at(&cfg, Instant::now()));
        let earlier = Instant::now() - Duration::from_secs(cfg.recovery_reconnect_probe_sec + 1);
        assert!(should_probe_at(&cfg, earlier));
    }
}
