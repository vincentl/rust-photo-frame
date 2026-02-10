mod config;
mod hotspot;
mod logging;
mod nm;
mod overlay;
mod password;
mod qr;
mod status;
mod watch;
mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(
    name = "wifi-manager",
    version,
    about = "Manage Wi-Fi provisioning and hotspot flow for the Photo Frame."
)]
struct Cli {
    /// Path to configuration file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Monitor connectivity and manage the hotspot + UI lifecycle.
    Watch,
    /// Run only the provisioning UI server.
    Ui,
    /// Generate the hotspot QR code asset.
    Qr,
    /// Low level NetworkManager helper subcommands.
    Nm {
        #[command(subcommand)]
        command: nm::NmCommand,
    },
    /// Launch the on-device recovery overlay window.
    Overlay(overlay::ui::OverlayCli),
}

#[tokio::main]
async fn main() {
    if let Err(err) = try_main().await {
        error!(error = ?err, "wifi-manager exited with error");
        std::process::exit(1);
    }
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();

    guard_root_usage()?;

    logging::init();

    let config_path = cli
        .config
        .unwrap_or_else(|| PathBuf::from("/opt/photoframe/etc/wifi-manager.yaml"));
    let config = Config::load(&config_path)?;
    // Updating the process environment is an unsafe operation on Rust 2024.
    // We only expose the configuration path to child processes, so the single
    // assignment here is contained and justified.
    unsafe {
        std::env::set_var("WIFI_MANAGER_CONFIG", &config_path);
    }

    info!(command = ?cli.command, config = %config_path.display(), "starting wifi-manager");

    match cli.command {
        Commands::Watch => watch::run(config.clone(), config_path).await?,
        Commands::Ui => web::run_ui(config).await?,
        Commands::Qr => qr::generate(&config)?,
        Commands::Nm { command } => nm::handle_cli(command, &config).await?,
        Commands::Overlay(args) => overlay::ui::run(args)?,
    }

    Ok(())
}

fn guard_root_usage() -> Result<()> {
    let uid = users::get_current_uid();
    if uid == 0 {
        let args: Vec<String> = std::env::args().collect();
        let is_help = args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"));
        let is_version = args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--version" | "-V"));
        if is_help || is_version {
            return Ok(());
        }
        anyhow::bail!("Refusing to run wifi-manager as root; run as the kiosk service user.");
    }
    Ok(())
}
