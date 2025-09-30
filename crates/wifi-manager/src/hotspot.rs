use crate::config::Config;
use crate::nm;
use crate::password;
use anyhow::{Context, Result};
use std::fs;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use tracing::info;

pub async fn activate(config: &Config) -> Result<Vec<String>> {
    let (password, words) = password::generate_from_wordlist(&config.wordlist_path, 3)?;
    nm::ensure_hotspot_profile(&config.hotspot, &config.interface, Some(&password)).await?;
    nm::bring_hotspot_up(&config.hotspot).await?;
    persist_password(config, &password)?;
    info!(ssid = %config.hotspot.ssid, "hotspot activated");
    Ok(words)
}

pub async fn deactivate(config: &Config) -> Result<()> {
    nm::bring_hotspot_down(&config.hotspot).await?;
    Ok(())
}

fn persist_password(config: &Config, password: &str) -> Result<()> {
    let path = hotspot_password_path(config);
    fs::create_dir_all(path.parent().unwrap())
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    use std::io::Write;
    file.write_all(password.as_bytes())
        .with_context(|| format!("failed to write hotspot password to {}", path.display()))?;
    Ok(())
}

pub fn hotspot_password_path(config: &Config) -> PathBuf {
    config.var_dir.join("hotspot-password.txt")
}
