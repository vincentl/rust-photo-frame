mod config;
// Shared with frametest (via lib.rs); this binary uses a subset.
#[allow(dead_code, unused_imports)]
mod drm_display;
mod events;
mod gpu;
mod processing;
mod renderer;
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
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[cfg(unix)]
use chrono::{DateTime, Utc};
#[cfg(unix)]
use config_model::control::{self, ErrorKind, Reply, Request};
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::UnixListener;

use events::{
    Displayed, InvalidPhoto, InventoryEvent, LoadPhoto, PhotoLoaded, ViewerCommand, ViewerState,
};

#[derive(Debug, Parser)]
#[command(
    name = "photoframe",
    version,
    about = "GPU-accelerated digital photo frame slideshow"
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
    /// Dry run with portrait pairing: the matting canvas pairability is judged
    /// on [default: 3840x2160 times global-photo-settings.oversample]
    #[arg(long = "canvas", value_name = "WxH", value_parser = parse_canvas)]
    canvas: Option<(u32, u32)>,
}

fn parse_canvas(value: &str) -> Result<(u32, u32), String> {
    let (w, h) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WxH, got {value:?}"))?;
    let parse = |s: &str| s.trim().parse::<u32>().ok().filter(|v| *v > 0);
    match (parse(w), parse(h)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(format!("expected positive WxH, got {value:?}")),
    }
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
        canvas,
    } = Args::parse();

    let now_override = match playlist_now {
        Some(ts) => Some(parse_rfc3339(&ts).context("failed to parse --playlist-now")?),
        None => None,
    };

    let cfg = Arc::new(
        config::Configuration::from_yaml_file(&config)
            .with_context(|| format!("failed to load configuration from {}", config.display()))?
            .validated()
            .context("invalid configuration values")?,
    );
    tracing::info!(
        "Loaded configuration from {}:\n{:#?}",
        config.display(),
        cfg
    );

    if let Some(iterations) = playlist_dry_run {
        run_playlist_dry_run(&cfg, iterations, now_override, playlist_seed, canvas)?;
        return Ok(());
    }

    // Decided before any task starts so a bad value fails fast.
    let surface = tasks::viewer::ViewerSurface::from_env()?;
    tracing::info!(?surface, "viewer surface");

    // Channels (bounded).  Capacities are chosen to bound memory while keeping the pipeline fluid:
    // - inv_tx: burst during startup scan can be large; 128 gives headroom without unbounded growth.
    // - invalid_tx / displayed_tx: low-frequency bookkeeping; 64 is generous.
    // - to_load_tx: intentionally small (4) so the manager stays in sync with what the loader needs.
    // - loaded_tx / processed_tx: sized to viewer_preload_count so the decode pipeline stays filled
    //   without buffering more decoded frames than the viewer will consume.
    // - viewer_control_tx: external control commands are rare; 16 is ample.
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(128);
    let (invalid_tx, invalid_rx) = mpsc::channel::<InvalidPhoto>(64);
    let (to_load_tx, to_load_rx) = mpsc::channel::<LoadPhoto>(4);
    let (loaded_tx, loaded_rx) = mpsc::channel::<PhotoLoaded>(cfg.viewer_preload_count);
    let (processed_tx, processed_rx) = mpsc::channel::<PhotoLoaded>(cfg.viewer_preload_count);
    let (displayed_tx, displayed_rx) = mpsc::channel::<Displayed>(64);
    let (viewer_control_tx, viewer_control_rx) = mpsc::channel::<ViewerCommand>(16);

    let cancel = CancellationToken::new();

    // Portrait pairing: the viewer publishes its canvas shape here; the
    // manager reads it at each pairing decision.
    let canvas_shape = events::CanvasShape::default();
    let live_pairing = tasks::manager::pairing::PairingRules::from_config(&cfg).map(|rules| {
        tasks::manager::pairing::LivePairing {
            rules,
            canvas: canvas_shape.clone(),
        }
    });

    // Both the stdin watcher and the Ctrl-C handler call cancel.cancel().
    // CancellationToken::cancel() is idempotent, so whichever fires first wins and
    // the second call is a no-op.
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

    // SIGTERM (systemctl stop, and every Restart= cycle) gets the same
    // graceful shutdown as Ctrl-C. Without it the default action kills the
    // process outright; on DRM/KMS the viewer must instead release the
    // swapchain, then the Vulkan instance, then the card (viewer/kms.rs).
    #[cfg(unix)]
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(term) => term,
                Err(err) => {
                    tracing::warn!("SIGTERM handler failed: {err}");
                    return;
                }
            };
            if term.recv().await.is_some() {
                tracing::info!("SIGTERM received; initiating shutdown");
                cancel.cancel();
            }
        });
    }

    let mut tasks = JoinSet::new();

    #[cfg(unix)]
    {
        let cancel = cancel.clone();
        let control = viewer_control_tx.clone();
        let control_socket_path = cfg.control_socket_path.clone();
        let greeting_delay = cfg.greeting_screen.effective_duration();
        let schedule = cfg.awake_schedule.clone();
        tasks.spawn(async move {
            run_control_socket(
                cancel,
                control,
                control_socket_path,
                greeting_delay,
                schedule,
                surface,
            )
            .await
            .context("control socket task failed")
        });
    }

    // PhotoFiles
    tasks.spawn({
        let cfg = Arc::clone(&cfg);
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
        let metrics = cfg.metrics;
        async move {
            tasks::manager::run(
                inv_rx,
                displayed_rx,
                to_load_tx,
                cancel,
                playlist,
                now_override,
                seed_override,
                metrics,
                live_pairing,
            )
            .await
            .context("manager task failed")
        }
    });

    // The viewer publishes its canvas size here once it has a surface; the loader
    // uses it to decode JPEGs at the smallest DCT scale that still covers it.
    // Portrait pairing's `CanvasShape` is published through the same call.
    let decode_scaling =
        tasks::loader::DecodeScaling::from_config(&cfg).with_canvas_shape(canvas_shape);

    // PhotoLoader
    tasks.spawn({
        let decode_scaling = decode_scaling.clone();
        let to_load_rx = to_load_rx;
        let invalid_tx = invalid_tx.clone();
        let loaded_tx = loaded_tx.clone();
        let cancel = cancel.clone();
        let max_in_flight = cfg.loader_max_concurrent_decodes;
        // Decoded sizes feed pairability; sent only while pairing is enabled.
        let meta_tx = cfg.playlist.pairing.enabled.then(|| inv_tx.clone());
        async move {
            tasks::loader::run(
                to_load_rx,
                invalid_tx,
                loaded_tx,
                cancel,
                max_in_flight,
                decode_scaling,
                meta_tx,
            )
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

    // Run the viewer on the main thread after spawning the other tasks. It
    // returns when the window closes, cancellation occurs, or (on KMS) the
    // display fails in a way that needs a restart.
    let viewer_result = match surface {
        tasks::viewer::ViewerSurface::Wayland => tasks::viewer::run_windowed(
            processed_rx,
            displayed_tx.clone(),
            cancel.clone(),
            Arc::clone(&cfg),
            viewer_control_rx,
            decode_scaling,
        ),
        tasks::viewer::ViewerSurface::Drm => {
            tasks::viewer::run_kms(
                processed_rx,
                displayed_tx.clone(),
                cancel.clone(),
                Arc::clone(&cfg),
                viewer_control_rx,
                decode_scaling,
            )
            .await
        }
    }
    .context("viewer failed");

    if let Err(err) = &viewer_result {
        tracing::error!("{err:?}");
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

    viewer_result
}

fn run_playlist_dry_run(
    cfg: &config::Configuration,
    iterations: usize,
    now_override: Option<SystemTime>,
    seed: Option<u64>,
    canvas: Option<(u32, u32)>,
) -> Result<()> {
    let now = now_override.unwrap_or_else(SystemTime::now);
    let mut photos = tasks::files::discover_startup_photos(cfg)?;
    let pairing = tasks::manager::pairing::PairingRules::from_config(cfg);
    // The matting canvas is the screen times the oversample factor.
    let canvas = canvas.unwrap_or_else(|| {
        let oversample = f64::from(cfg.global_photo_settings.oversample);
        (
            (3840.0 * oversample).round().max(1.0) as u32,
            (2160.0 * oversample).round().max(1.0) as u32,
        )
    });
    if pairing.is_some() {
        // The dry run reads headers inline.
        for info in &mut photos {
            info.display_size = processing::header::read_display_size(&info.path);
        }
    }

    println!(
        "# playlist dry run\n# photos: {}\n# now: {}\n# iterations: {}\n# seed: {}\n# order: {} (min-spacing {:.3})\n",
        photos.len(),
        format_rfc3339(now),
        iterations,
        seed.map_or_else(|| "(random)".to_string(), |s| s.to_string()),
        cfg.playlist.order,
        cfg.playlist.refractory_fraction(),
    );

    if photos.is_empty() {
        println!(
            "(no photos discovered under {})",
            cfg.photo_library_path.display()
        );
        return Ok(());
    }

    println!("# weights (relative show frequency; equilibrium = 1.0):");
    for info in &photos {
        let weight = cfg.playlist.weight_for(info.created_at, now);
        println!("  {:>5.2} × {}", weight, info.path.display());
    }

    if let Some(rules) = pairing {
        return print_paired_dry_run(cfg, &photos, &rules, canvas, now, iterations, seed);
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

/// The dry run with portrait pairing on: screens (pairs as `a + b`) and a
/// `# pairing` block of measured values (design §2 "Dry run").
fn print_paired_dry_run(
    cfg: &config::Configuration,
    photos: &[events::PhotoInfo],
    rules: &tasks::manager::pairing::PairingRules,
    canvas: (u32, u32),
    now: SystemTime,
    iterations: usize,
    seed: Option<u64>,
) -> Result<()> {
    use std::collections::HashMap;
    use tasks::manager::planner::ScreenStats;

    let pairing = &cfg.playlist.pairing;
    println!(
        "\n# pairing: probability {} bar-ratio {} dwell-multiplier {}",
        pairing.probability, pairing.bar_ratio, pairing.dwell_multiplier
    );
    println!(
        "# canvas: {}x{} (max-upscale {}, non-zero mats {:.1}%..{:.1}%)",
        canvas.0,
        canvas.1,
        rules.max_upscale,
        rules.m_min * 100.0,
        rules.m_max * 100.0
    );

    let pairable: HashMap<&std::path::Path, bool> = photos
        .iter()
        .map(|info| {
            let ok = rules.is_pairable(info.display_size, Some(canvas));
            (info.path.as_path(), ok)
        })
        .collect();
    let is_pairable = |path: &std::path::Path| pairable.get(path).copied().unwrap_or(false);

    let screens = tasks::manager::simulate_screens(
        photos.iter().cloned(),
        cfg.playlist.clone(),
        Some(rules),
        Some(canvas),
        now,
        iterations,
        seed,
    );

    println!("\n# planned screens:");
    if screens.is_empty() {
        println!("(playlist empty)");
    }
    for (idx, screen) in screens.iter().enumerate() {
        match &screen.partner {
            Some(partner) => println!(
                "  {:>4}: {} + {}",
                idx + 1,
                screen.primary.path.display(),
                partner.path.display()
            ),
            None => println!("  {:>4}: {}", idx + 1, screen.primary.path.display()),
        }
    }

    // π: the weight share of pairable photos.
    let (mut weight_pairable, mut weight_total) = (0.0, 0.0);
    for info in photos {
        let weight = cfg.playlist.weight_for(info.created_at, now);
        weight_total += weight;
        if is_pairable(&info.path) {
            weight_pairable += weight;
        }
    }
    let pi = weight_pairable / f64::max(weight_total, f64::MIN_POSITIVE);
    let stats = ScreenStats::measure(&screens, &is_pairable);
    println!("\n# pairing");
    println!(
        "#   pairable photos: {} of {}",
        pairable.values().filter(|ok| **ok).count(),
        photos.len()
    );
    println!("#   pi (pairable weight share): {pi:.3}");
    println!(
        "#   screens: {} for {} appearances ({} pairs)",
        stats.screens, stats.appearances, stats.pair_screens
    );
    println!(
        "#   paired share of pairable appearances: {:.3} (2*pi*p/(1+pi*p) = {:.3}, an approximation)",
        stats.paired_share,
        ScreenStats::predicted_share(pi, rules.probability)
    );
    println!("#   repeat-partner rate: {:.3}", stats.repeat_partner_rate);
    println!(
        "#   consecutive-screen repeats: {} (must be 0 unless the unpaired sequence repeats a photo back to back; I5)",
        stats.consecutive_repeats
    );
    Ok(())
}

#[cfg(unix)]
impl From<control::ViewerState> for ViewerState {
    fn from(value: control::ViewerState) -> Self {
        match value {
            control::ViewerState::Awake => ViewerState::Awake,
            control::ViewerState::Asleep => ViewerState::Asleep,
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
        if let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(path = %self.path.display(), ?err, "failed to remove control socket");
        }
    }
}

#[cfg(unix)]
async fn run_control_socket(
    cancel: CancellationToken,
    control: mpsc::Sender<ViewerCommand>,
    socket_path: PathBuf,
    greeting_delay: Duration,
    schedule: Option<config::AwakeScheduleConfig>,
    surface: tasks::viewer::ViewerSurface,
) -> Result<()> {
    if let Some(parent) = socket_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
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

    if let Err(err) =
        run_initial_schedule_preamble(&cancel, &control, greeting_delay, schedule.as_ref()).await
    {
        tracing::warn!("control preamble failed: {err}");
    }

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
                            if let Err(err) = handle_control_connection(stream, control, surface).await {
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
async fn run_initial_schedule_preamble(
    cancel: &CancellationToken,
    control: &mpsc::Sender<ViewerCommand>,
    greeting_delay: Duration,
    schedule: Option<&config::AwakeScheduleConfig>,
) -> Result<()> {
    if !greeting_delay.is_zero() {
        tracing::debug!(
            delay = %humantime::format_duration(greeting_delay),
            "waiting for greeting preamble before applying schedule",
        );
        let delay = tokio::time::sleep(greeting_delay);
        tokio::pin!(delay);
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = delay => {}
        }
    }

    if cancel.is_cancelled() {
        return Ok(());
    }

    let target_state = schedule
        .map(|schedule| scheduled_viewer_state(schedule, Utc::now()))
        .unwrap_or(ViewerState::Awake);

    tracing::info!(
        ?target_state,
        "control preamble sending initial viewer state"
    );
    control
        .send(ViewerCommand::SetState(target_state))
        .await
        .context("failed to send initial viewer state")?;

    Ok(())
}

#[cfg(unix)]
fn scheduled_viewer_state(
    schedule: &config::AwakeScheduleConfig,
    now: DateTime<Utc>,
) -> ViewerState {
    let local_now = now.with_timezone(&schedule.timezone());
    if schedule.is_awake_at(local_now) {
        ViewerState::Awake
    } else {
        ViewerState::Asleep
    }
}

#[cfg(all(test, unix))]
mod schedule_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn scheduled_viewer_state_respects_intervals() {
        let yaml = r#"
timezone: "UTC"
awake-scheduled:
  daily:
    - ["06:00", "08:00"]
    - ["10:00", "22:00"]
"#;

        let schedule: config::AwakeScheduleConfig =
            serde_yaml::from_str(yaml).expect("valid schedule");

        let awake_morning = Utc
            .with_ymd_and_hms(2024, 5, 1, 7, 30, 0)
            .earliest()
            .expect("valid time");
        assert_eq!(
            scheduled_viewer_state(&schedule, awake_morning),
            ViewerState::Awake
        );

        let asleep_gap = Utc
            .with_ymd_and_hms(2024, 5, 1, 9, 0, 0)
            .earliest()
            .expect("valid time");
        assert_eq!(
            scheduled_viewer_state(&schedule, asleep_gap),
            ViewerState::Asleep
        );

        let awake_evening = Utc
            .with_ymd_and_hms(2024, 5, 1, 12, 0, 0)
            .earliest()
            .expect("valid time");
        assert_eq!(
            scheduled_viewer_state(&schedule, awake_evening),
            ViewerState::Awake
        );
    }
}

/// A client gets this long to send its request and half-close.
#[cfg(unix)]
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the control task waits for the viewer to answer a command only
/// it can answer (display power). The viewer handles commands between
/// frames, and a DPMS commit waits at most a refresh for a pending flip.
#[cfg(unix)]
const VIEWER_REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// Requests are small; anything bigger is not one.
#[cfg(unix)]
const MAX_CONTROL_PAYLOAD: u64 = 64 * 1024;

/// One connection: read the request until the client half-closes, act on
/// it, write one reply line (see `config_model::control` for the framing).
/// A client that does not read the reply (buttond writes and drops) is not
/// an error: its command has already been applied.
#[cfg(unix)]
async fn handle_control_connection(
    mut stream: tokio::net::UnixStream,
    control: mpsc::Sender<ViewerCommand>,
    surface: tasks::viewer::ViewerSurface,
) -> Result<()> {
    let mut buf = Vec::with_capacity(256);
    let mut limited = (&mut stream).take(MAX_CONTROL_PAYLOAD);
    let read = limited.read_to_end(&mut buf);
    match tokio::time::timeout(CONTROL_READ_TIMEOUT, read).await {
        Ok(result) => {
            result.context("failed to read control command")?;
        }
        Err(_) => {
            tracing::warn!(
                "control client sent no complete request within {CONTROL_READ_TIMEOUT:?}"
            );
            return Ok(());
        }
    }

    if buf.is_empty() {
        tracing::debug!("ignoring empty control payload");
        return Ok(());
    }

    let reply = answer_control_request(&buf, &control, surface).await;
    // Best effort: an old client may already have closed its end.
    let _ = stream.write_all(reply.to_line().as_bytes()).await;
    let _ = stream.shutdown().await;
    Ok(())
}

/// Parse one request, act on it, and say how it went. Never logs the
/// payload itself: an overlay request carries the hotspot password.
#[cfg(unix)]
async fn answer_control_request(
    payload: &[u8],
    control: &mpsc::Sender<ViewerCommand>,
    surface: tasks::viewer::ViewerSurface,
) -> Reply {
    let request: Request = match serde_json::from_slice(payload) {
        Ok(request) => request,
        Err(err) => {
            let command = control::command_name(payload);
            tracing::warn!(
                command = command.as_deref().unwrap_or("?"),
                "invalid control payload: {err}"
            );
            return Reply::error(ErrorKind::Invalid, err.to_string());
        }
    };
    let name = request.name();
    let forward = |cmd: ViewerCommand| async move {
        match control.send(cmd).await {
            Ok(()) => Reply::ok(),
            Err(_) => Reply::error(ErrorKind::Failed, "the viewer is shutting down"),
        }
    };

    match request {
        Request::ToggleState => {
            tracing::info!(command = name, "received control command");
            forward(ViewerCommand::ToggleState).await
        }
        Request::SetState { state } => {
            let viewer_state: ViewerState = state.into();
            tracing::info!(command = name, ?viewer_state, "received control command");
            forward(ViewerCommand::SetState(viewer_state)).await
        }
        Request::DisplayPower { state, output } => {
            tracing::info!(
                command = name,
                state = state.map_or("query", control::PowerState::as_str),
                output = output.as_deref().unwrap_or("*"),
                "received control command"
            );
            if surface == tasks::viewer::ViewerSurface::Wayland {
                // The compositor owns the output; powerctl falls back to
                // swaymsg on this answer, which is the Wayland behaviour.
                return Reply::error(
                    ErrorKind::Unsupported,
                    "the compositor owns display power on this stack",
                );
            }
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let sent = control
                .send(ViewerCommand::DisplayPower {
                    state,
                    output,
                    reply: reply_tx,
                })
                .await;
            if sent.is_err() {
                return Reply::error(ErrorKind::Failed, "the viewer is shutting down");
            }
            match tokio::time::timeout(VIEWER_REPLY_TIMEOUT, reply_rx).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => Reply::error(ErrorKind::Failed, "the viewer dropped the request"),
                Err(_) => Reply::error(
                    ErrorKind::Timeout,
                    format!("no answer from the viewer within {VIEWER_REPLY_TIMEOUT:?}"),
                ),
            }
        }
        Request::WifiOverlayShow(overlay) => {
            if let Err(why) = tasks::viewer::validate_wifi_overlay(&overlay) {
                tracing::warn!(command = name, "rejected: {why}");
                return Reply::error(ErrorKind::Invalid, why);
            }
            // Refreshed every few seconds while shown; the viewer logs the
            // changes that matter.
            tracing::debug!(command = name, ssid = %overlay.ssid, "received control command");
            forward(ViewerCommand::ShowWifiOverlay(Box::new(overlay))).await
        }
        Request::WifiOverlayHide => {
            tracing::info!(command = name, "received control command");
            forward(ViewerCommand::HideWifiOverlay).await
        }
    }
}

#[cfg(all(test, unix))]
mod control_socket_tests {
    use super::*;
    use tasks::viewer::ViewerSurface;
    use tokio::net::{UnixListener, UnixStream};

    /// A listener in a temp dir that runs `handle_control_connection` for
    /// each connection, like `run_control_socket` does.
    async fn serve(
        surface: ViewerSurface,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        mpsc::Receiver<ViewerCommand>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = handle_control_connection(stream, tx, surface).await;
                });
            }
        });
        (dir, path, rx)
    }

    /// Write, half-close, read the reply line: what powerctl's socat does.
    async fn request(path: &std::path::Path, payload: &str) -> Reply {
        let mut stream = UnixStream::connect(path).await.expect("connect");
        stream.write_all(payload.as_bytes()).await.expect("write");
        stream.shutdown().await.expect("half-close");
        let mut line = String::new();
        stream.read_to_string(&mut line).await.expect("read reply");
        serde_json::from_str(line.trim_end()).expect("reply is JSON")
    }

    #[tokio::test]
    async fn set_state_is_forwarded_and_answered() {
        let (_dir, path, mut rx) = serve(ViewerSurface::Drm).await;
        let reply = request(&path, r#"{"command":"set-state","state":"asleep"}"#).await;
        assert_eq!(reply, Reply::ok());
        assert!(matches!(
            rx.recv().await,
            Some(ViewerCommand::SetState(ViewerState::Asleep))
        ));
    }

    #[tokio::test]
    async fn a_client_that_writes_and_drops_still_delivers() {
        // buttond: write_all, then drop the stream without reading.
        let (_dir, path, mut rx) = serve(ViewerSurface::Drm).await;
        {
            let mut stream = UnixStream::connect(&path).await.expect("connect");
            stream
                .write_all(br#"{"command":"toggle-state"}"#)
                .await
                .expect("write");
        }
        assert!(matches!(rx.recv().await, Some(ViewerCommand::ToggleState)));
    }

    #[tokio::test]
    async fn display_power_carries_the_viewer_answer() {
        let (_dir, path, mut rx) = serve(ViewerSurface::Drm).await;
        let viewer = tokio::spawn(async move {
            match rx.recv().await {
                Some(ViewerCommand::DisplayPower {
                    state,
                    output,
                    reply,
                }) => {
                    assert_eq!(state, Some(control::PowerState::Off));
                    assert_eq!(output.as_deref(), Some("HDMI-A-2"));
                    let _ = reply.send(Reply::power(
                        control::PowerState::Off,
                        "HDMI-A-2",
                        "3840x2160@60",
                    ));
                }
                other => panic!("unexpected command {other:?}"),
            }
        });
        let reply = request(
            &path,
            r#"{"command":"display-power","state":"off","output":"HDMI-A-2"}"#,
        )
        .await;
        viewer.await.expect("viewer task");
        assert_eq!(
            reply,
            Reply::power(control::PowerState::Off, "HDMI-A-2", "3840x2160@60")
        );
    }

    #[tokio::test]
    async fn display_power_is_the_compositors_on_wayland() {
        let (_dir, path, mut rx) = serve(ViewerSurface::Wayland).await;
        let reply = request(&path, r#"{"command":"display-power","state":"off"}"#).await;
        assert!(!reply.ok);
        assert_eq!(reply.error, Some(ErrorKind::Unsupported));
        assert!(rx.try_recv().is_err(), "nothing reaches the viewer");
    }

    #[tokio::test(start_paused = true)]
    async fn a_viewer_that_never_answers_times_out() {
        let (tx, mut rx) = mpsc::channel(4);
        let answer = tokio::spawn(async move {
            answer_control_request(br#"{"command":"display-power"}"#, &tx, ViewerSurface::Drm).await
        });
        // Hold the reply sender without answering.
        let held = rx.recv().await;
        let reply = answer.await.expect("answer task");
        drop(held);
        assert_eq!(reply.error, Some(ErrorKind::Timeout));
    }

    #[tokio::test]
    async fn invalid_payloads_are_answered_not_forwarded() {
        let (_dir, path, mut rx) = serve(ViewerSurface::Drm).await;
        for payload in [
            r#"{"command":"ToggleSleep"}"#,
            r#"{"command":"set-state","state":"sleep"}"#,
            "not json",
        ] {
            let reply = request(&path, payload).await;
            assert_eq!(reply.error, Some(ErrorKind::Invalid), "{payload}");
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn an_overlay_out_of_limits_is_rejected_by_field() {
        let (_dir, path, mut rx) = serve(ViewerSurface::Drm).await;
        let payload = serde_json::json!({
            "command": "wifi-overlay-show",
            "headline": "h", "body": "b", "ssid": "s",
            "password": "secret-words-here",
            "join-qr": "", "portal-url": "http://192.168.4.1:8080/", "footer": "f"
        })
        .to_string();
        let reply = request(&path, &payload).await;
        assert_eq!(reply.error, Some(ErrorKind::Invalid));
        let detail = reply.detail.unwrap_or_default();
        assert!(
            detail.contains("join-qr") && !detail.contains("secret"),
            "{detail}"
        );
        assert!(rx.try_recv().is_err());
    }
}
