mod config;
mod events;
mod processing;
mod tasks {
    pub mod files;
    pub mod loader;
    pub mod manager;
    pub mod viewer;
}

use anyhow::{Context, Result};
use clap::Parser;
use std::io::{self, Read};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use events::{Displayed, InvalidPhoto, InventoryEvent, LoadPhoto, PhotoLoaded};

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

    let Args { config } = Args::parse();
    let cfg = config::Configuration::from_yaml_file(&config)
        .with_context(|| format!("failed to load configuration from {}", config.display()))?
        .validated()
        .context("invalid configuration values")?;
    tracing::info!(
        "Loaded configuration from {}:\n{:#?}",
        config.display(),
        cfg
    );

    // Channels (small/bounded)
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(128); // Files -> Manager
    let (invalid_tx, invalid_rx) = mpsc::channel::<InvalidPhoto>(64); // Manager/Loader -> Files
    let (to_load_tx, to_load_rx) = mpsc::channel::<LoadPhoto>(4); // Manager -> Loader (allow a few in-flight requests)
    let (loaded_tx, loaded_rx) = mpsc::channel::<PhotoLoaded>(cfg.viewer_preload_count); // Loader  -> Viewer (prefetch up to cfg.viewer_preload_count)
    let (displayed_tx, displayed_rx) = mpsc::channel::<Displayed>(64); // Viewer  -> Manager

    let cancel = CancellationToken::new();

    // Ctrl-D/Ctrl-C cancel the pipeline
    {
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let mut sink = Vec::new();
            match io::stdin().read_to_end(&mut sink) {
                Ok(_) => tracing::info!("stdin closed; initiating shutdown"),
                Err(err) => tracing::warn!("stdin watcher failed: {err}"),
            }
            cancel.cancel();
        });
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
        async move {
            tasks::manager::run(inv_rx, displayed_rx, to_load_tx, cancel, playlist)
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

    // Run the windowed viewer on the main thread (blocking) after spawning other tasks
    // This call returns when the window closes or cancellation occurs
    if let Err(e) =
        tasks::viewer::run_windowed(loaded_rx, displayed_tx.clone(), cancel.clone(), cfg.clone())
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
