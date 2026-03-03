use crate::config::Config;
use crate::hotspot::hotspot_password_path;
use anyhow::{Context, Result};
use image::Luma;
use qrcode::QrCode;
use std::fs;
use std::path::PathBuf;

/// Generate a QR code that encodes Wi-Fi join credentials in the `WIFI:` URI
/// format recognised by iOS 11+ (camera app) and Android 10+ (camera/settings).
///
/// Scanning pops up a one-tap "Join Network" prompt — no manual password
/// entry needed.  The portal URL QR is shown separately so the user can scan
/// to join the hotspot and then scan to open the setup page.
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

    let code = QrCode::new(wifi_uri.as_bytes()).context("failed to generate Wi-Fi join QR code")?;
    let image = code.render::<Luma<u8>>().min_dimensions(256, 256).build();
    let path = wifi_qr_path(config);
    fs::create_dir_all(&config.var_dir)
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;
    image
        .save(&path)
        .with_context(|| format!("failed to write Wi-Fi join QR code to {}", path.display()))?;
    Ok(())
}

/// Generate a QR code that encodes the portal setup URL so users can scan to
/// open the Wi-Fi configuration page without typing the address manually.
pub fn generate_portal_qr(config: &Config) -> Result<()> {
    let url = format!("http://{}:{}/", config.hotspot.ipv4_addr, config.ui.port);
    let code = QrCode::new(url.as_bytes()).context("failed to generate portal URL QR code")?;
    let image = code.render::<Luma<u8>>().min_dimensions(256, 256).build();
    let path = portal_qr_path(config);
    fs::create_dir_all(&config.var_dir)
        .with_context(|| format!("failed to create var dir at {}", config.var_dir.display()))?;
    image
        .save(&path)
        .with_context(|| format!("failed to write portal QR code to {}", path.display()))?;
    Ok(())
}

pub fn wifi_qr_path(config: &Config) -> PathBuf {
    config.var_dir.join("wifi-qr.png")
}

pub fn portal_qr_path(config: &Config) -> PathBuf {
    config.var_dir.join("portal-qr.png")
}

/// Kept for callers that previously used `qr_path` — now an alias for
/// `wifi_qr_path`.
#[inline]
pub fn qr_path(config: &Config) -> PathBuf {
    wifi_qr_path(config)
}
