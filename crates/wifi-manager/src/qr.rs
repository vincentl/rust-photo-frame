use crate::config::Config;
use anyhow::{Context, Result};
use image::Luma;
use qrcode::QrCode;
use std::fs;

pub fn generate(config: &Config) -> Result<()> {
    let url = format!("http://{}:{}/", config.hotspot.ipv4_addr, config.ui.port);
    let code = QrCode::new(url.as_bytes()).context("failed to generate QR code")?;
    let image = code.render::<Luma<u8>>().min_dimensions(256, 256).build();
    let path = config.var_dir.join("wifi-qr.png");
    fs::create_dir_all(&config.var_dir)
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;
    image
        .save(&path)
        .with_context(|| format!("failed to write QR code to {}", path.display()))?;
    Ok(())
}

pub fn qr_path(config: &Config) -> std::path::PathBuf {
    config.var_dir.join("wifi-qr.png")
}
