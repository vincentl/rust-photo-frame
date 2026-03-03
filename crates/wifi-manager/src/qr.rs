use crate::config::Config;
use crate::hotspot::hotspot_password_path;
use anyhow::{Context, Result};
use image::Luma;
use qrcode::QrCode;
use std::fs;

/// Generate a QR code that encodes Wi-Fi join credentials in the `WIFI:` URI
/// format recognised by iOS 11+ (camera app) and Android 10+ (camera/settings).
///
/// Scanning pops up a one-tap "Join Network" prompt — no manual password
/// entry needed.  The portal URL is already displayed as readable text on the
/// overlay, so the user opens it after joining.
///
/// Format: `WIFI:T:WPA;S:<ssid>;P:<password>;;`
/// Characters `\`, `;`, `,`, `"` would need escaping, but our SSID
/// (`PhotoFrame-Setup`) and three-word passwords never contain them.
pub fn generate(config: &Config) -> Result<()> {
    let password_path = hotspot_password_path(config);
    let password = fs::read_to_string(&password_path).with_context(|| {
        format!(
            "failed to read hotspot password from {}",
            password_path.display()
        )
    })?;
    let password = password.trim();

    let wifi_uri = format!(
        "WIFI:T:WPA;S:{ssid};P:{password};;",
        ssid = config.hotspot.ssid,
        password = password,
    );

    let code = QrCode::new(wifi_uri.as_bytes()).context("failed to generate QR code")?;
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
