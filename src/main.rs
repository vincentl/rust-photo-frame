mod config;
mod scan;
mod meta;
mod render;
mod index;
mod watch;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(short, long)]
    config: String,

    #[arg(short, long, default_value = "info")]
    log: String,

    /// Skip EXIF to speed up scanning
    #[arg(long)]
    no_exif: bool,

    /// Start filesystem watcher and keep process alive
    #[arg(long)]
    watch: bool,
}

fn init_logging(level: &str) {
    let env = EnvFilter::try_new(level).or_else(|_| EnvFilter::try_new("info")).unwrap();
    fmt().with_env_filter(env).init();
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(&args.log);

    let cfg_text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("reading config: {}", &args.config))?;
    let cfg = config::Config::from_yaml(&cfg_text).context("parsing YAML config")?;
    cfg.validate().context("validating config")?;

    // Initial scan (fast)
    let files = scan::scan_dirs(&cfg.photo_paths);

    // (Optional) start notify as you implemented earlier…
    // For the demo, just feed the list to the slideshow:
    if files.is_empty() {
        eprintln!("No supported images to show.");
        return Ok(());
    }

    render::viewer::run_slideshow(files)?;

    Ok(())
}
