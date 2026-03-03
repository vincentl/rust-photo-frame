use crate::config::Config;
use crate::nm;
use crate::password;
use anyhow::{Context, Result};
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

pub async fn activate(config: &Config) -> Result<Vec<String>> {
    let (password, words) = password::generate_from_wordlist(&config.wordlist_path, 3)?;
    // Force a profile restart before applying credentials so NetworkManager
    // doesn't keep serving an older active AP key across repeated recovery runs.
    if let Err(err) = nm::bring_hotspot_down(&config.hotspot).await {
        warn!(error = ?err, "failed to bring hotspot down before password refresh");
    }
    nm::ensure_hotspot_profile(&config.hotspot, &config.interface, Some(&password)).await?;
    // Persist before launching the AP so overlay rendering and portal guidance
    // always source the same password we just wrote into NetworkManager.
    persist_password(config, &password)?;
    nm::bring_hotspot_up(&config.hotspot).await?;
    info!(ssid = %config.hotspot.ssid, "hotspot activated");
    Ok(words)
}

pub async fn deactivate(config: &Config) -> Result<()> {
    nm::bring_hotspot_down(&config.hotspot).await?;
    Ok(())
}

fn persist_password(config: &Config, password: &str) -> Result<()> {
    let path = hotspot_password_path(config);
    let parent = path
        .parent()
        .context("hotspot password path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;

    // Write to a temp file and atomically rename so readers never observe an
    // empty/truncated hotspot password during refresh.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let temp_path = parent.join(format!(
        ".hotspot-password.{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp_path)
        .with_context(|| format!("failed to open {}", temp_path.display()))?;
    use std::io::Write;
    file.write_all(password.as_bytes()).with_context(|| {
        format!(
            "failed to write hotspot password to {}",
            temp_path.display()
        )
    })?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    drop(file);

    fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", temp_path.display()))?;
    fs::rename(&temp_path, &path).with_context(|| {
        format!(
            "failed to atomically replace hotspot password {}",
            path.display()
        )
    })?;
    Ok(())
}

pub fn hotspot_password_path(config: &Config) -> PathBuf {
    config.var_dir.join("hotspot-password.txt")
}
