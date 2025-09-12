//! Minimal binary that delegates to the `photoframe` library API.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

use photoframe::{DisplayOptions, build_buffer, config, run_slideshow, scan_photos};

/// Photoframe – minimal slideshow runner.
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Path to YAML configuration file.
    #[arg(short, long, value_name = "FILE", default_value = "config.yaml")]
    config: PathBuf,
}

fn main() -> anyhow::Result<()> {
    // logging
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();

    let args = Args::parse();

    let cfg = config::from_yaml_file(&args.config)
        .with_context(|| format!("loading config from {}", args.config.display()))?;

    let mut buffer = {
        let photos = scan_photos(&cfg).context("scanning photo directories")?;
        build_buffer(photos).context("building photo buffer")?
    };

    let display = DisplayOptions::from(cfg.display());

    run_slideshow(&mut buffer, &display).context("running slideshow")?;

    Ok(())
}
