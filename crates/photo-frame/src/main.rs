mod config;
mod events;
mod gpu;
mod processing;
mod schedule;
mod tasks {
    pub mod files;
    pub mod greeting_screen;
    pub mod loader;
    pub mod manager;
    pub mod photo_effect;
    pub mod viewer;
}

use anyhow::{Context, Result};
use clap::Parser;
use humantime::{format_rfc3339, parse_rfc3339};
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[cfg(unix)]
use anyhow::anyhow;
#[cfg(unix)]
use serde::Deserialize;
#[cfg(unix)]
use tokio::io::AsyncReadExt;
#[cfg(unix)]
use tokio::net::UnixListener;

use events::{
    Displayed, InvalidPhoto, InventoryEvent, LoadPhoto, PhotoLoaded, ViewerCommand, ViewerState,
};

#[derive(Debug, Parser)]
#[command(
    name = "rust-photo-frame",
    version,
    about = "photo frame minimal scaffold"
)]
struct Args {
    /// Path to YAML config
    #[arg(value_name = "CONFIG")]
    config: PathBuf,
    /// Freeze playlist weighting at this RFC 3339 instant (useful for tests)
    #[arg(long = "playlist-now", value_name = "RFC3339")]
    playlist_now: Option<String>,
    /// Print the weighted playlist order without launching the UI
    #[arg(long = "playlist-dry-run", value_name = "ITERATIONS")]
    playlist_dry_run: Option<usize>,
    /// Deterministic RNG seed for playlist shuffling (applies to dry-run and live modes)
    #[arg(long = "playlist-seed", value_name = "SEED")]
    playlist_seed: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // init tracing (RUST_LOG controls level, default = info)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let Args {
        config,
        playlist_now,
        playlist_dry_run,
        playlist_seed,
    } = Args::parse();

    let now_override = match playlist_now {
        Some(ts) => Some(parse_rfc3339(&ts).context("failed to parse --playlist-now")?),
        None => None,
    };

    let cfg = config::Configuration::from_yaml_file(&config)
        .with_context(|| format!("failed to load configuration from {}", config.display()))?
        .validated()
        .context("invalid configuration values")?;
    tracing::info!(
        "Loaded configuration from {}:\n{:#?}",
        config.display(),
        cfg
    );

    if let Some(iterations) = playlist_dry_run {
        run_playlist_dry_run(&cfg, iterations, now_override, playlist_seed)?;
        return Ok(());
    }

    // Channels (small/bounded)
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(128); // Files -> Manager
    let (invalid_tx, invalid_rx) = mpsc::channel::<InvalidPhoto>(64); // Manager/Loader -> Files
    let (to_load_tx, to_load_rx) = mpsc::channel::<LoadPhoto>(4); // Manager -> Loader (allow a few in-flight requests)
    let (loaded_tx, loaded_rx) = mpsc::channel::<PhotoLoaded>(cfg.viewer_preload_count); // Loader -> PhotoEffect
    let (processed_tx, processed_rx) = mpsc::channel::<PhotoLoaded>(cfg.viewer_preload_count); // PhotoEffect -> Viewer
    let (displayed_tx, displayed_rx) = mpsc::channel::<Displayed>(64); // Viewer  -> Manager
    let (viewer_control_tx, viewer_control_rx) = mpsc::channel::<ViewerCommand>(16); // External -> Viewer

    let cancel = CancellationToken::new();

    // Ctrl-D/Ctrl-C cancel the pipeline
    if io::stdin().is_terminal() {
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let mut sink = Vec::new();
            match io::stdin().read_to_end(&mut sink) {
                Ok(_) => tracing::info!("stdin closed; initiating shutdown"),
                Err(err) => tracing::warn!("stdin watcher failed: {err}"),
            }
            cancel.cancel();
        });
    } else {
        tracing::debug!("stdin is not a terminal; skipping shutdown watcher");
    }

    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(err) = tokio::signal::ctrl_c().await {
                tracing::warn!("ctrl-c handler failed: {err}");
                return;
            }
            tracing::info!("ctrl-c received; initiating shutdown");
            cancel.cancel();
        });
    }

    let mut tasks = JoinSet::new();

    #[cfg(unix)]
    {
        let cancel = cancel.clone();
        let control = viewer_control_tx.clone();
        let control_socket_path = cfg.control_socket_path.clone();
        tasks.spawn(async move {
            run_control_socket(cancel, control, control_socket_path)
                .await
                .context("control socket task failed")
        });
    }

    let greeting_duration = cfg.greeting_screen.effective_duration();

    if let Some(schedule_cfg) = cfg.awake_schedule.clone() {
        let cancel = cancel.clone();
        let control = viewer_control_tx.clone();
        let greeting_duration = greeting_duration;
        tasks.spawn(async move {
            schedule::run(schedule_cfg, cancel, control, greeting_duration)
                .await
                .context("awake schedule task failed")
        });
    } else {
        let cancel = cancel.clone();
        let control = viewer_control_tx.clone();
        let greeting_duration = greeting_duration;
        let greeting_duration_ms = greeting_duration.as_millis().min(u128::from(u64::MAX)) as u64;
        tasks.spawn(async move {
            tracing::debug!(greeting_duration_ms, "auto_wake_timer_armed");
            tokio::select! {
                _ = cancel.cancelled() => Ok(()),
                _ = sleep(greeting_duration) => {
                    tracing::debug!(greeting_duration_ms, "auto_wake_timer_elapsed");
                    if let Err(err) = control.send(ViewerCommand::SetState(ViewerState::Awake)).await {
                        tracing::debug!("auto-wake command dropped: {err}");
                    } else {
                        tracing::debug!("auto_wake_command_sent");
                    }
                    Ok(())
                }
            }
        });
    }

    // PhotoFiles
    tasks.spawn({
        let cfg = cfg.clone();
        let inv_tx = inv_tx.clone();
        let invalid_rx = invalid_rx;
        let cancel = cancel.clone();
        async move {
            tasks::files::run(cfg, inv_tx, invalid_rx, cancel)
                .await
                .context("files task failed")
        }
    });

    // PhotoManager
    tasks.spawn({
        let inv_rx = inv_rx;
        let displayed_rx = displayed_rx;
        let to_load_tx = to_load_tx.clone();
        let cancel = cancel.clone();
        let playlist = cfg.playlist.clone();
        let seed_override = playlist_seed;
        async move {
            tasks::manager::run(
                inv_rx,
                displayed_rx,
                to_load_tx,
                cancel,
                playlist,
                now_override,
                seed_override,
            )
            .await
            .context("manager task failed")
        }
    });

    // PhotoLoader
    tasks.spawn({
        let to_load_rx = to_load_rx;
        let invalid_tx = invalid_tx.clone();
        let loaded_tx = loaded_tx.clone();
        let cancel = cancel.clone();
        let max_in_flight = cfg.loader_max_concurrent_decodes;
        async move {
            tasks::loader::run(to_load_rx, invalid_tx, loaded_tx, cancel, max_in_flight)
                .await
                .context("loader task failed")
        }
    });

    // PhotoEffect pipeline (optional post-processing)
    let photo_effect_cfg = cfg.photo_effect.clone();
    tasks.spawn({
        let from_loader = loaded_rx;
        let to_viewer = processed_tx.clone();
        let cancel = cancel.clone();
        let effect_cfg = photo_effect_cfg;
        async move {
            tasks::photo_effect::run(from_loader, to_viewer, cancel, effect_cfg)
                .await
                .context("photo-effect task failed")
        }
    });

    // Run the windowed viewer on the main thread (blocking) after spawning other tasks
    // This call returns when the window closes or cancellation occurs
    if let Err(e) = tasks::viewer::run_windowed(
        processed_rx,
        displayed_tx.clone(),
        cancel.clone(),
        cfg.clone(),
        viewer_control_rx,
    )
    .context("viewer failed")
    {
        tracing::error!("{e:?}");
    }
    // Ensure other tasks are asked to stop
    cancel.cancel();

    // Drain JoinSet (wait for other tasks to complete)
    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("task error: {e:?}"),
            Err(e) => tracing::error!("join error: {e}"),
        }
    }

    Ok(())
}

fn run_playlist_dry_run(
    cfg: &config::Configuration,
    iterations: usize,
    now_override: Option<SystemTime>,
    seed: Option<u64>,
) -> Result<()> {
    let now = now_override.unwrap_or_else(SystemTime::now);
    let photos = tasks::files::discover_startup_photos(cfg)?;

    println!(
        "# playlist dry run\n# photos: {}\n# now: {}\n# iterations: {}\n# seed: {}\n",
        photos.len(),
        format_rfc3339(now),
        iterations,
        seed.map_or_else(|| "(random)".to_string(), |s| s.to_string())
    );

    if photos.is_empty() {
        println!(
            "(no photos discovered under {})",
            cfg.photo_library_path.display()
        );
        return Ok(());
    }

    println!("# weights (multiplicity per cycle):");
    for info in &photos {
        let multiplicity = cfg.playlist.multiplicity_for(info.created_at, now);
        println!("  {:>3} × {}", multiplicity, info.path.display());
    }

    let plan = tasks::manager::simulate_playlist(
        photos.clone(),
        cfg.playlist.clone(),
        now,
        iterations,
        seed,
    );

    println!("\n# planned order:");
    if plan.is_empty() {
        println!("(playlist empty)");
    } else {
        for (idx, path) in plan.iter().enumerate() {
            println!("  {:>4}: {}", idx + 1, path.display());
        }
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "command")]
enum ControlCommand {
    #[serde(rename = "toggle-state")]
    ToggleState,
    #[serde(rename = "set-state")]
    SetState { state: ControlState },
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ControlState {
    Awake,
    Asleep,
}

#[cfg(unix)]
impl From<ControlState> for ViewerState {
    fn from(value: ControlState) -> Self {
        match value {
            ControlState::Awake => ViewerState::Awake,
            ControlState::Asleep => ViewerState::Asleep,
        }
    }
}

#[cfg(unix)]
struct SocketCleanup {
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %self.path.display(), ?err, "failed to remove control socket");
            }
        }
    }
}

#[cfg(unix)]
async fn run_control_socket(
    cancel: CancellationToken,
    control: mpsc::Sender<ViewerCommand>,
    socket_path: PathBuf,
) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                return Err(err).with_context(|| {
                    format!(
                        "failed to create control socket directory {parent:?}; ensure the directory is writable or set control-socket-path"
                    )
                });
            }
            return Err(err)
                .with_context(|| format!("failed to create control socket directory {parent:?}"));
        }
    }

    if socket_path.exists() {
        match std::fs::remove_file(&socket_path) {
            Ok(_) => tracing::warn!(path = %socket_path.display(), "removed stale control socket"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to remove stale control socket at {}",
                        socket_path.display()
                    )
                });
            }
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind control socket at {}", socket_path.display()))?;
    let _cleanup = SocketCleanup {
        path: socket_path.clone(),
    };

    tracing::info!(path = %socket_path.display(), "listening for control commands");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("shutdown requested; closing control socket");
                break;
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let control = control.clone();
                        tokio::spawn(async move {
                            if let Err(err) = handle_control_connection(stream, control).await {
                                tracing::warn!("control connection failed: {err}");
                            }
                        });
                    }
                    Err(err) => {
                        if err.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        return Err(err).context("failed to accept control connection");
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
const MAX_CONTROL_PAYLOAD_BYTES: usize = 16 * 1024;

#[cfg(unix)]
async fn handle_control_connection(
    mut stream: tokio::net::UnixStream,
    control: mpsc::Sender<ViewerCommand>,
) -> Result<()> {
    let mut buf = Vec::with_capacity(128);

    loop {
        let bytes_read = stream
            .read_buf(&mut buf)
            .await
            .context("failed to read control command")?;

        if bytes_read == 0 {
            break;
        }

        if buf.len() > MAX_CONTROL_PAYLOAD_BYTES {
            tracing::warn!(
                payload_len = buf.len(),
                limit = MAX_CONTROL_PAYLOAD_BYTES,
                "control payload exceeds limit"
            );
            return Err(anyhow!(
                "control payload exceeds {} bytes",
                MAX_CONTROL_PAYLOAD_BYTES
            ));
        }
    }

    if buf.is_empty() {
        tracing::debug!("ignoring empty control payload");
        return Ok(());
    }

    let payload_text = String::from_utf8_lossy(&buf);

    let request: ControlCommand = match serde_json::from_slice(&buf) {
        Ok(command) => command,
        Err(err) => {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&buf) {
                match value.get("command").and_then(|c| c.as_str()) {
                    None => {
                        tracing::warn!(payload = %payload_text, "control payload missing `command`: {err}")
                    }
                    Some("set-state") => match value.get("state") {
                        None => {
                            tracing::warn!(payload = %payload_text, "set-state command missing `state`: {err}");
                        }
                        Some(serde_json::Value::String(state)) => match state.as_str() {
                            "awake" | "asleep" => {
                                tracing::warn!(payload = %payload_text, state = %state, "invalid set-state payload: {err}");
                            }
                            _ => {
                                tracing::warn!(payload = %payload_text, state = %state, "unrecognized viewer state (expected `awake` or `asleep`): {err}");
                            }
                        },
                        Some(_) => {
                            tracing::warn!(payload = %payload_text, "set-state command expects string `state`: {err}");
                        }
                    },
                    Some("toggle-state") => {
                        tracing::warn!(payload = %payload_text, command = "toggle-state", "malformed toggle command payload: {err}");
                    }
                    Some(command) => {
                        tracing::warn!(payload = %payload_text, command, "unsupported control command");
                    }
                }
            } else {
                tracing::warn!(payload = %payload_text, "invalid control payload: {err}");
            }
            return Ok(());
        }
    };

    match request {
        ControlCommand::ToggleState => {
            tracing::info!(command = "toggle-state", "received control command");
            control
                .send(ViewerCommand::ToggleState)
                .await
                .context("failed to forward toggle-state command")?;
        }
        ControlCommand::SetState { state } => {
            let viewer_state: ViewerState = state.into();
            tracing::info!(
                command = "set-state",
                ?viewer_state,
                "received control command"
            );
            control
                .send(ViewerCommand::SetState(viewer_state))
                .await
                .context("failed to forward set-state command")?;
        }
    }

    Ok(())
}

#[cfg(unix)]
mod tests {
    #[test]
    fn deserialize_toggle_state_command() {
        let cmd: super::ControlCommand = serde_json::from_str(r#"{"command":"toggle-state"}"#)
            .expect("toggle-state should parse");
        assert_eq!(cmd, super::ControlCommand::ToggleState);
    }

    #[test]
    fn deserialize_set_state_awake() {
        let cmd: super::ControlCommand =
            serde_json::from_str(r#"{"command":"set-state","state":"awake"}"#)
                .expect("set-state should parse");

        assert_eq!(
            cmd,
            super::ControlCommand::SetState {
                state: super::ControlState::Awake
            }
        );
    }

    #[test]
    fn deserialize_set_state_asleep() {
        let cmd: super::ControlCommand =
            serde_json::from_str(r#"{"command":"set-state","state":"asleep"}"#)
                .expect("set-state should parse");

        assert_eq!(
            cmd,
            super::ControlCommand::SetState {
                state: super::ControlState::Asleep
            }
        );
    }

    #[test]
    fn deserialize_toggle_state_alias_rejected() {
        serde_json::from_str::<super::ControlCommand>(r#"{"command":"ToggleSleep"}"#)
            .expect_err("ToggleSleep should be rejected");
    }

    #[test]
    fn deserialize_set_state_alias_rejected() {
        serde_json::from_str::<super::ControlCommand>(r#"{"command":"set-state","state":"sleep"}"#)
            .expect_err("set-state sleep alias should be rejected");
    }

    #[test]
    fn deserialize_set_state_rejects_unknown() {
        let err = serde_json::from_str::<super::ControlCommand>(
            r#"{"command":"set-state","state":"invalid"}"#,
        )
        .expect_err("invalid state should fail");

        let message = err.to_string();
        assert!(message.contains("unknown variant"));
        assert!(message.contains("awake"));
        assert!(message.contains("asleep"));
    }

    #[tokio::test]
    async fn control_connection_rejects_large_payloads() {
        use tokio::io::AsyncWriteExt;

        let (mut client, server) =
            tokio::net::UnixStream::pair().expect("unix stream pair should create");

        let limit = super::MAX_CONTROL_PAYLOAD_BYTES;
        let payload = vec![b'a'; limit + 1];
        client
            .write_all(&payload)
            .await
            .expect("should write payload");
        client.shutdown().await.expect("should shutdown");

        let (tx, _rx) = tokio::sync::mpsc::channel(1);

        let err = super::handle_control_connection(server, tx)
            .await
            .expect_err("payload should be rejected");

        let message = err.to_string();
        assert!(message.contains("control payload exceeds"));
    }
}
