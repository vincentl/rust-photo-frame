mod config;
mod events;
mod tasks {
    pub mod files;
    pub mod loader;
    pub mod manager;
    pub mod viewer;
}

use anyhow::Result;
use clap::Parser;
use std::io::{self, Read};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use events::{InvalidPhoto, InventoryEvent, LoadPhoto, PhotoLoaded};

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
    let Args { config } = Args::parse();
    let cfg = config::Configuration::from_yaml_file(&config)?;
    println!(
        "Loaded configuration from {}:\n{:#?}",
        config.display(),
        cfg
    );

    // Channels (small/bounded)
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(8); // PhotoFiles -> Manager
    let (invalid_tx, invalid_rx) = mpsc::channel::<InvalidPhoto>(8); // Manager/Loader -> PhotoFiles
    let (to_load_tx, to_load_rx) = mpsc::channel::<LoadPhoto>(2); // Manager -> Loader (short)
    let (loaded_tx, loaded_rx) = mpsc::channel::<PhotoLoaded>(2); // Loader  -> Viewer (short)

    let cancel = CancellationToken::new();

    // Ctrl-D cancels
    {
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let mut sink = Vec::new();
            let _ = io::stdin().read_to_end(&mut sink);
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
        async move { tasks::files::run(cfg, inv_tx, invalid_rx, cancel).await }
    });

    // PhotoManager
    tasks.spawn({
        let inv_rx = inv_rx;
        let invalid_tx = invalid_tx.clone();
        let to_load_tx = to_load_tx.clone();
        let cancel = cancel.clone();
        async move { tasks::manager::run(inv_rx, invalid_tx, to_load_tx, cancel).await }
    });

    // PhotoLoader
    tasks.spawn({
        let to_load_rx = to_load_rx;
        let invalid_tx = invalid_tx.clone();
        let loaded_tx = loaded_tx.clone();
        let cancel = cancel.clone();
        async move { tasks::loader::run(to_load_rx, invalid_tx, loaded_tx, cancel).await }
    });

    // PhotoViewer
    tasks.spawn({
        let loaded_rx = loaded_rx;
        let cancel = cancel.clone();
        async move { tasks::viewer::run(loaded_rx, cancel).await }
    });

    // Drain JoinSet
    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("task error: {e:?}"),
            Err(e) => eprintln!("join error: {e}"),
        }
    }

    Ok(())
}
