mod config;
mod events;
mod platform;
mod processing;
mod tasks {
    pub mod files;
    pub mod greeting_screen;
    pub mod loader;
    pub mod manager;
    pub mod photo_effect;
    pub mod viewer;
}

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use humantime::{format_rfc3339, parse_rfc3339};
use std::borrow::Cow;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

use events::{Displayed, InvalidPhoto, InventoryEvent, LoadPhoto, PhotoLoaded, ViewerCommand};
use platform::display_power::PowerCommandReport;
use tokio::time::{sleep as tokio_sleep, Duration as TokioDuration};

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
    /// Run the configured display-power sleep path for N seconds and exit
    #[arg(long = "sleep-test", value_name = "SECONDS")]
    sleep_test: Option<u64>,
    /// Log the parsed sleep schedule and the next 24h of transitions at startup
    #[arg(long = "verbose-sleep")]
    verbose_sleep: bool,
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
        sleep_test,
        verbose_sleep,
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

    if verbose_sleep {
        if let Some(runtime) = cfg.sleep_mode.as_ref().and_then(|cfg| cfg.runtime()) {
            log_sleep_timeline(runtime);
        } else {
            tracing::info!("--verbose-sleep requested but sleep-mode is not configured");
        }
    }

    if let Some(seconds) = sleep_test {
        let runtime = cfg
            .sleep_mode
            .as_ref()
            .and_then(|cfg| cfg.runtime())
            .ok_or_else(|| {
                anyhow!("--sleep-test requires sleep-mode.display-power configuration")
            })?;
        run_sleep_test(runtime, seconds).await?;
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

    #[cfg(unix)]
    {
        let cancel = cancel.clone();
        let control = viewer_control_tx.clone();
        tokio::spawn(async move {
            match signal(SignalKind::user_defined1()) {
                Ok(mut sigusr1) => loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        received = sigusr1.recv() => {
                            if received.is_none() {
                                break;
                            }
                            tracing::info!("SIGUSR1 received; toggling sleep mode");
                            if let Err(err) = control.send(ViewerCommand::ToggleSleep).await {
                                tracing::warn!("failed to forward sleep toggle request: {err}");
                                break;
                            }
                        }
                    }
                },
                Err(err) => tracing::warn!("failed to register SIGUSR1 handler: {err}"),
            }
        });
    }

    let mut tasks = JoinSet::new();

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

fn describe_schedule_source_main(source: config::ScheduleSource) -> Cow<'static, str> {
    match source {
        config::ScheduleSource::Default => Cow::Borrowed("on-hours"),
        config::ScheduleSource::WeekdayOverride => Cow::Borrowed("weekday-override"),
        config::ScheduleSource::WeekendOverride => Cow::Borrowed("weekend-override"),
        config::ScheduleSource::DayOverride(day) => {
            Cow::Owned(format!("days.{}", day.to_string().to_ascii_lowercase()))
        }
    }
}

fn log_sleep_timeline(runtime: &config::SleepModeRuntime) {
    let now = Utc::now();
    let snapshot = runtime.schedule_snapshot(now);
    let state = if snapshot.awake { "awake" } else { "sleeping" };
    let source = describe_schedule_source_main(snapshot.active_source);
    tracing::info!(
        timezone = runtime.base_timezone().to_string(),
        state,
        schedule_source = source.as_ref(),
        local_time = snapshot.now_local.to_rfc3339(),
        "sleep schedule snapshot"
    );

    let transitions = runtime.upcoming_transitions(now, ChronoDuration::hours(24));
    if transitions.is_empty() {
        tracing::info!("no sleep transitions in the next 24h");
    } else {
        for boundary in transitions {
            let state = if boundary.awake { "awake" } else { "sleeping" };
            let source = describe_schedule_source_main(boundary.source);
            tracing::info!(
                transition_local = boundary.at_local.to_rfc3339(),
                state,
                source = source.as_ref(),
                weekday = ?boundary.weekday,
                "sleep transition planned"
            );
        }
    }
}

fn log_power_report_main(attempt: &str, report: &PowerCommandReport) {
    let output_name = report.output.as_ref().map(|sel| sel.name.as_str());
    let output_source = report
        .output
        .as_ref()
        .map(|sel| format!("{:?}", sel.source));
    let stderr_messages: Vec<&str> = report
        .commands
        .iter()
        .filter(|cmd| !cmd.stderr.trim().is_empty())
        .map(|cmd| cmd.stderr.as_str())
        .collect();
    let stderr_combined = if stderr_messages.is_empty() {
        None
    } else {
        Some(stderr_messages.join("; "))
    };

    if report.success() {
        tracing::info!(
            action = ?report.action,
            attempt,
            output = output_name,
            output_source = output_source.as_deref(),
            "display power action succeeded"
        );
    } else {
        tracing::warn!(
            action = ?report.action,
            attempt,
            output = output_name,
            output_source = output_source.as_deref(),
            stderr = stderr_combined.as_deref(),
            "display power action failed"
        );
    }

    for cmd in &report.commands {
        tracing::debug!(
            action = ?report.action,
            attempt,
            command = cmd.command,
            success = cmd.success,
            exit_code = cmd.exit_code,
            stderr = cmd.stderr,
            stdout = cmd.stdout,
            "display power command detail"
        );
    }

    for sysfs in &report.sysfs {
        tracing::debug!(
            action = ?report.action,
            attempt,
            path = %sysfs.path.display(),
            value = sysfs.value,
            success = sysfs.success,
            error = ?sysfs.error,
            "display power sysfs detail"
        );
    }
}

async fn run_sleep_test(runtime: &config::SleepModeRuntime, seconds: u64) -> Result<()> {
    let controller = runtime
        .display_power()
        .cloned()
        .ok_or_else(|| anyhow!("sleep-test requires sleep-mode.display-power configuration"))?;

    tracing::info!(duration = seconds, "sleep-test: requesting sleep");
    let sleep_report = controller.sleep();
    log_power_report_main("sleep-test", &sleep_report);
    if !sleep_report.success() {
        bail!("sleep-test sleep command failed");
    }

    tokio_sleep(TokioDuration::from_secs(seconds)).await;

    tracing::info!("sleep-test: requesting wake");
    let wake_report = controller.wake();
    log_power_report_main("wake-test-1", &wake_report);
    if !wake_report.success() {
        tracing::warn!("sleep-test wake attempt 1 failed; retrying");
        tokio_sleep(TokioDuration::from_secs(2)).await;
        let retry = controller.wake();
        log_power_report_main("wake-test-2", &retry);
        if !retry.success() {
            bail!("sleep-test wake command failed after retry");
        }
    }

    tracing::info!("sleep-test completed successfully");
    Ok(())
}
