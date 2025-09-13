mod config;

use anyhow::Result;
use clap::Parser;
use std::io::{self, Read};
use std::path::PathBuf;

/// Minimal driver: read YAML config, print, wait for Ctrl-D.
#[derive(Debug, Parser)]
#[command(name = "rust-photo-frame", version, about = "Photo frame minimal driver")]
struct Args {
    /// Path to YAML config
    #[arg(value_name = "CONFIG")]
    config: PathBuf,
}

fn main() -> Result<()> {
    let Args { config: cfg_path } = Args::parse();

    let cfg = config::Configuration::from_yaml_file(&cfg_path)?;
    println!("Loaded configuration from {}:\n{:#?}", cfg_path.display(), cfg);
    println!("Press Ctrl-D to exit...");

    let mut sink = Vec::new();
    io::stdin().read_to_end(&mut sink)?;
    Ok(())
}
