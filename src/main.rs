//! Binary entrypoint for Photoframe.
//!
//! Delegates all logic to the library crate; no local modules here.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use tracing::{Level, info};
use tracing_subscriber::{EnvFilter, fmt};

/// Simple CLI
#[derive(Debug, Parser)]
#[command(name = "rust-photo-frame", about = "Rust-based photo frame")]
struct Cli {
    /// Path to YAML config file
    #[arg(short, long, value_name = "FILE", default_value = "config.yaml")]
    config: PathBuf,

    /// Override per-image delay (ms)
    #[arg(long, value_name = "MILLIS")]
    delay_ms: Option<u64>,

    /// Increase log verbosity (repeatable)
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count)]
    verbose: u8,
}

fn init_tracing(verbosity: u8) -> Result<()> {
    // map -v to log level
    let level = match verbosity {
        0 => Level::INFO,
        1 => Level::DEBUG,
        _ => Level::TRACE,
    };
    let filter = EnvFilter::from_default_env()
        .add_directive(format!("rust_photo_frame={}", level).parse().unwrap())
        .add_directive("wgpu=warn".parse().unwrap())
        .add_directive("winit=warn".parse().unwrap());
    fmt().with_env_filter(filter).with_target(true).init();
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose)?;

    // Use the library crate only.
    let cfg = photoframe::config::from_yaml_file(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;
    cfg.validate().context("validating configuration")?;

    let photos = photoframe::scan_photos(&cfg)?;
    info!(count = photos.len(), "scanned images");

    // Build circular buffer and run
    let mut buf = photoframe::build_buffer(photos)?;
    let display = if let Some(ms) = cli.delay_ms {
        photoframe::DisplayOptions { delay_ms: ms }
    } else {
        photoframe::DisplayOptions::from(cfg.display())
    };

    photoframe::run_slideshow(&mut buf, &display)?;
    Ok(())
}
