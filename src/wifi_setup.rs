use std::future::IntoFuture;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use crossbeam_channel::{unbounded, Receiver, Sender};
use image::{imageops, ImageBuffer, Rgba, RgbaImage};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use softbuffer::{Context as SoftContext, Surface as SoftSurface};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::WifiSetupConfig;

const DISPLAY_WIDTH: u32 = 1080;
const DISPLAY_HEIGHT: u32 = 1920;
const BACKGROUND_COLOR: [u8; 4] = [16, 20, 24, 255];
const TEXT_COLOR: [u8; 4] = [230, 235, 240, 255];
const ACCENT_COLOR: [u8; 4] = [86, 160, 255, 255];
const ERROR_COLOR: [u8; 4] = [255, 112, 112, 255];

#[derive(Debug, Clone, PartialEq, Eq)]
enum DisplayMessage {
    Idle,
    Applying { ssid: String },
    Failed { ssid: String, reason: String },
    Connected { ssid: String },
}

impl Default for DisplayMessage {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone)]
struct SetupCoordinator {
    cfg: WifiSetupConfig,
    status_tx: Sender<DisplayMessage>,
    cancel: CancellationToken,
    applying: Arc<AsyncMutex<bool>>,
    success: Arc<AtomicBool>,
}

#[derive(Debug, Serialize)]
struct JoinResponse {
    status: &'static str,
    message: &'static str,
}

#[derive(Debug, Deserialize)]
struct JoinRequest {
    ssid: String,
    password: String,
}

pub enum EnsureResult {
    AlreadyConnected,
    RestartRequired,
}

pub async fn ensure_connected(cfg: &WifiSetupConfig) -> Result<EnsureResult> {
    if !cfg.enabled {
        return Ok(EnsureResult::AlreadyConnected);
    }

    match is_wifi_connected(&cfg.wifi_interface).await {
        Ok(Some(_)) => return Ok(EnsureResult::AlreadyConnected),
        Ok(None) => {}
        Err(err) => {
            warn!("wifi status check failed ({err:?}); skipping setup fallback");
            return Ok(EnsureResult::AlreadyConnected);
        }
    }

    info!("no active wifi connection detected; starting setup portal");
    run_setup(cfg.clone()).await
}

async fn run_setup(cfg: WifiSetupConfig) -> Result<EnsureResult> {
    let (status_tx, status_rx) = unbounded::<DisplayMessage>();
    let cancel = CancellationToken::new();
    let success = Arc::new(AtomicBool::new(false));
    let coordinator = Arc::new(SetupCoordinator {
        cfg: cfg.clone(),
        status_tx: status_tx.clone(),
        cancel: cancel.clone(),
        applying: Arc::new(AsyncMutex::new(false)),
        success: success.clone(),
    });

    status_tx.send(DisplayMessage::Idle).ok();

    if let Err(err) = start_hotspot(&cfg).await {
        warn!("failed to start setup hotspot: {err:?}");
    }

    let listener = TcpListener::bind((cfg.portal_bind_address.as_str(), cfg.portal_port))
        .await
        .with_context(|| {
            format!(
                "failed to bind wifi setup portal at {}:{}",
                cfg.portal_bind_address, cfg.portal_port
            )
        })?;

    let router = Router::new()
        .route("/", get(index_handler))
        .route("/join", post(join_handler))
        .with_state(coordinator.clone());

    let cancel_for_server = cancel.clone();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(cancel_for_server.cancelled_owned())
        .into_future();
    let server_handle = tokio::spawn(server);

    let display_cfg = cfg.clone();
    let display_cancel = cancel.clone();
    let display_handle = std::thread::spawn(move || {
        if let Err(err) = run_display(&display_cfg, status_rx, display_cancel) {
            error!("wifi setup display failed: {err:?}");
        }
    });

    // Wait for successful connection
    cancel.clone().cancelled().await;

    drop(status_tx);
    match server_handle.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("wifi setup server exited with error: {err:?}"),
        Err(err) => warn!("wifi setup server join error: {err:?}"),
    }
    let _ = display_handle.join();

    if success.load(Ordering::SeqCst) {
        stop_hotspot(&cfg).await.ok();
        Ok(EnsureResult::RestartRequired)
    } else {
        Ok(EnsureResult::AlreadyConnected)
    }
}

async fn index_handler(State(state): State<Arc<SetupCoordinator>>) -> Html<String> {
    let cfg = &state.cfg;
    let url = cfg.portal_url();
    let body = format!(
        r#"<!DOCTYPE html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Frame Wi-Fi Setup</title>
<style>
body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; margin: 0; padding: 24px; background: #0f1418; color: #eef2f5; }}
main {{ max-width: 480px; margin: 0 auto; }}
h1 {{ font-size: 2rem; margin-bottom: 0.5rem; }}
section {{ margin-top: 1.5rem; }}
label {{ display: block; margin-bottom: 0.5rem; font-weight: 600; }}
input {{ width: 100%; padding: 0.75rem; margin-bottom: 1rem; border-radius: 8px; border: 1px solid #29323a; background: #151c22; color: inherit; }}
button {{ width: 100%; padding: 0.9rem; background: #3b82f6; color: white; border: none; border-radius: 10px; font-size: 1rem; font-weight: 600; cursor: pointer; }}
button:disabled {{ background: #1f2933; cursor: not-allowed; }}
#status {{ margin-top: 1rem; min-height: 1.5rem; font-weight: 500; }}
.code {{ font-family: 'SFMono-Regular', ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace; background: #1b242c; padding: 0.4rem 0.6rem; border-radius: 6px; }}
</style>
</head>
<body>
<main>
<h1>Connect this frame to Wi-Fi</h1>
<p>Your frame is broadcasting <span class=\"code\">{ssid}</span> with password <span class=\"code\">{password}</span>. Join that network and submit your home Wi-Fi credentials below.</p>
<section>
<label for=\"ssid\">Wi-Fi network name (SSID)</label>
<input id=\"ssid\" placeholder=\"MyHomeNetwork\" autocomplete=\"off\" required />
<label for=\"password\">Wi-Fi password</label>
<input id=\"password\" type=\"password\" placeholder=\"password\" autocomplete=\"off\" required />
<button id=\"submit\" type=\"button\">Join Wi-Fi</button>
<div id=\"status\"></div>
</section>
<p>If things stall, reconnect to <span class=\"code\">{ssid}</span> and try again. You can also visit <span class=\"code\">{url}</span> directly. Once the frame connects it will reboot automatically.</p>
</main>
<script>
const button = document.getElementById('submit');
const status = document.getElementById('status');
async function submit() {{
  const ssid = document.getElementById('ssid').value.trim();
  const password = document.getElementById('password').value;
  if (!ssid) {{
    status.textContent = 'Enter a Wi-Fi network name to continue.';
    return;
  }}
  button.disabled = true;
  status.textContent = 'Applying credentials…';
  try {{
    const res = await fetch('/join', {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json' }},
      body: JSON.stringify({{ ssid, password }})
    }});
    const data = await res.json();
    status.textContent = data.message;
  }} catch (err) {{
    status.textContent = 'Failed to submit credentials. Please retry.';
  }} finally {{
    button.disabled = false;
  }}
}}
button.addEventListener('click', submit);
</script>
</body>
</html>
"#,
        ssid = cfg.hotspot_ssid,
        password = cfg.hotspot_password,
        url = url,
    );
    Html(body)
}

async fn join_handler(
    State(state): State<Arc<SetupCoordinator>>,
    Json(payload): Json<JoinRequest>,
) -> impl IntoResponse {
    let ssid = payload.ssid.trim().to_string();
    if ssid.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(JoinResponse {
                status: "error",
                message: "SSID must not be empty",
            }),
        );
    }

    let mut guard = state.applying.lock().await;
    if *guard {
        return (
            StatusCode::CONFLICT,
            Json(JoinResponse {
                status: "busy",
                message: "Already applying credentials, please wait",
            }),
        );
    }
    *guard = true;
    drop(guard);

    let coordinator = state.clone();
    let password = payload.password.clone();
    state
        .status_tx
        .send(DisplayMessage::Applying { ssid: ssid.clone() })
        .ok();

    tokio::spawn(async move {
        if let Err(err) = attempt_join(coordinator.clone(), ssid.clone(), password).await {
            warn!("wifi join attempt failed: {err:?}");
            coordinator
                .status_tx
                .send(DisplayMessage::Failed {
                    ssid: ssid.clone(),
                    reason: err.to_string(),
                })
                .ok();
            if let Err(err) = start_hotspot(&coordinator.cfg).await {
                warn!("failed to re-enable hotspot after error: {err:?}");
            }
        }
        *coordinator.applying.lock().await = false;
    });

    (
        StatusCode::OK,
        Json(JoinResponse {
            status: "accepted",
            message:
                "Attempting to join the specified Wi-Fi network. The frame will reboot on success.",
        }),
    )
}

async fn attempt_join(state: Arc<SetupCoordinator>, ssid: String, password: String) -> Result<()> {
    stop_hotspot(&state.cfg).await.ok();

    update_wpa_supplicant(&state.cfg.wpa_supplicant_path, &ssid, &password)
        .await
        .context("failed to update wpa_supplicant")?;

    reconfigure_wpa(&state.cfg).await?;

    wait_for_wifi(&state.cfg, &ssid).await?;

    state
        .status_tx
        .send(DisplayMessage::Connected { ssid: ssid.clone() })
        .ok();
    state.success.store(true, Ordering::SeqCst);
    info!("wifi setup completed; connected to {ssid}");
    state.cancel.cancel();
    Ok(())
}

async fn update_wpa_supplicant(path: &PathBuf, ssid: &str, password: &str) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    let escaped_ssid = escape_wpa_string(ssid);
    let escaped_password = escape_wpa_string(password);
    let block = if password.is_empty() {
        format!("\nnetwork={{\n    ssid=\"{escaped_ssid}\"\n    key_mgmt=NONE\n}}\n")
    } else {
        format!("\nnetwork={{\n    ssid=\"{escaped_ssid}\"\n    psk=\"{escaped_password}\"\n}}\n")
    };
    tokio::io::AsyncWriteExt::write_all(&mut file, block.as_bytes()).await?;
    Ok(())
}

fn escape_wpa_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

async fn reconfigure_wpa(cfg: &WifiSetupConfig) -> Result<()> {
    let mut cmd = match &cfg.wpa_cli_path {
        Some(path) => Command::new(path),
        None => Command::new("wpa_cli"),
    };
    let status = cmd
        .arg("-i")
        .arg(&cfg.wifi_interface)
        .arg("reconfigure")
        .status()
        .await
        .context("failed to run wpa_cli")?;
    if !status.success() {
        bail!("wpa_cli exited with status {status}");
    }
    Ok(())
}

async fn wait_for_wifi(cfg: &WifiSetupConfig, target_ssid: &str) -> Result<()> {
    let mut remaining = cfg.connection_check_timeout_secs;
    loop {
        if let Some(connected) = is_wifi_connected(&cfg.wifi_interface).await? {
            if connected == target_ssid {
                return Ok(());
            }
        }
        if remaining == 0 {
            bail!("device did not report a Wi-Fi connection in time");
        }
        let sleep_for = cfg.connection_check_interval_secs.min(remaining);
        remaining = remaining.saturating_sub(sleep_for);
        sleep(Duration::from_secs(sleep_for)).await;
    }
}

async fn is_wifi_connected(interface: &str) -> Result<Option<String>> {
    let output = Command::new("iwgetid")
        .arg(interface)
        .arg("-r")
        .output()
        .await;
    match output {
        Ok(out) => {
            if !out.status.success() {
                return Ok(None);
            }
            let ssid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if ssid.is_empty() {
                Ok(None)
            } else {
                Ok(Some(ssid))
            }
        }
        Err(err) => Err(anyhow!("failed to execute iwgetid: {err}")),
    }
}

async fn start_hotspot(cfg: &WifiSetupConfig) -> Result<()> {
    if cfg.hotspot_start_command.is_empty() {
        return Ok(());
    }
    run_command(&cfg.hotspot_start_command).await
}

async fn stop_hotspot(cfg: &WifiSetupConfig) -> Result<()> {
    if cfg.hotspot_stop_command.is_empty() {
        return Ok(());
    }
    run_command(&cfg.hotspot_stop_command).await
}

async fn run_command(command: &[String]) -> Result<()> {
    if command.is_empty() {
        return Ok(());
    }
    let mut iter = command.iter();
    let program = iter
        .next()
        .ok_or_else(|| anyhow!("command vector must contain program"))?;
    let mut cmd = Command::new(program);
    for arg in iter {
        cmd.arg(arg);
    }
    let status = cmd.status().await?;
    if !status.success() {
        bail!("command {program} exited with {status}");
    }
    Ok(())
}

fn run_display(
    cfg: &WifiSetupConfig,
    status_rx: Receiver<DisplayMessage>,
    cancel: CancellationToken,
) -> Result<()> {
    use std::time::Instant;
    use winit::dpi::PhysicalSize;
    use winit::event::{Event, WindowEvent};
    use winit::event_loop::{ControlFlow, EventLoop};
    use winit::window::WindowAttributes;

    let event_loop = EventLoop::new()?;
    #[allow(deprecated)]
    let window = event_loop.create_window(
        WindowAttributes::default()
            .with_title("Wi-Fi Setup")
            .with_inner_size(PhysicalSize::new(DISPLAY_WIDTH, DISPLAY_HEIGHT))
            .with_resizable(false),
    )?;

    let context = SoftContext::new(&window)
        .map_err(|err| anyhow!("failed to create softbuffer context: {err:?}"))?;
    let mut surface = SoftSurface::new(&context, &window)
        .map_err(|err| anyhow!("failed to create softbuffer surface: {err:?}"))?;
    surface
        .resize(
            NonZeroU32::new(DISPLAY_WIDTH).expect("display width must be non-zero"),
            NonZeroU32::new(DISPLAY_HEIGHT).expect("display height must be non-zero"),
        )
        .map_err(|err| anyhow!("failed to resize softbuffer surface: {err:?}"))?;

    let cfg_owned = cfg.clone();
    let status_rx = status_rx;
    let mut current_image = render_screen(&cfg_owned, &DisplayMessage::Idle)?;

    #[allow(deprecated)]
    let result = event_loop.run(move |event, event_loop| {
        let wake_at = Instant::now() + Duration::from_millis(200);
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
        if cancel.is_cancelled() {
            event_loop.exit();
            return;
        }
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                event_loop.exit();
            }
            Event::AboutToWait => {
                while let Ok(new_status) = status_rx.try_recv() {
                    current_image = render_screen(&cfg_owned, &new_status).unwrap_or_else(|err| {
                        error!("failed to render wifi setup screen: {err:?}");
                        current_image.clone()
                    });
                }
                match surface.buffer_mut() {
                    Ok(mut buffer) => {
                        let len = buffer.len().min(current_image.len());
                        buffer[..len].copy_from_slice(&current_image[..len]);
                        if let Err(err) = buffer.present() {
                            error!("softbuffer present error: {err:?}");
                        }
                    }
                    Err(err) => {
                        error!("softbuffer buffer access error: {err:?}");
                    }
                }
            }
            _ => {}
        }
    });
    if let Err(err) = result {
        return Err(anyhow!("event loop error: {err}"));
    }
    Ok(())
}

fn render_screen(cfg: &WifiSetupConfig, message: &DisplayMessage) -> Result<Vec<u32>> {
    let mut canvas: RgbaImage =
        ImageBuffer::from_pixel(DISPLAY_WIDTH, DISPLAY_HEIGHT, Rgba(BACKGROUND_COLOR));

    let font = FontArc::try_from_slice(include_bytes!("../assets/fonts/Inconsolata-Regular.ttf"))
        .context("failed to load embedded font")?;

    let title_scale = PxScale::from(72.0);
    draw_text(
        &mut canvas,
        &font,
        title_scale,
        80,
        180,
        TEXT_COLOR,
        "Wi-Fi Setup",
    );

    let portal_url = cfg.portal_url();
    let qr = QrCode::new(portal_url.as_bytes()).context("failed to generate QR code")?;
    let qr_image = qr
        .render::<image::Luma<u8>>()
        .min_dimensions(280, 280)
        .build();
    let qr_rgba = qr_to_rgba(&qr_image);
    imageops::overlay(&mut canvas, &qr_rgba, (DISPLAY_WIDTH as i64 / 2) - 140, 260);

    draw_text(
        &mut canvas,
        &font,
        PxScale::from(40.0),
        100,
        620,
        ACCENT_COLOR,
        "Scan the code or visit:",
    );
    draw_text(
        &mut canvas,
        &font,
        PxScale::from(44.0),
        100,
        700,
        TEXT_COLOR,
        &portal_url,
    );

    draw_text(
        &mut canvas,
        &font,
        PxScale::from(38.0),
        100,
        820,
        TEXT_COLOR,
        &format!("Hotspot SSID: {}", cfg.hotspot_ssid),
    );
    draw_text(
        &mut canvas,
        &font,
        PxScale::from(38.0),
        100,
        880,
        TEXT_COLOR,
        &format!("Password: {}", cfg.hotspot_password),
    );

    let status_text = match message {
        DisplayMessage::Idle => "Waiting for credentials…".to_string(),
        DisplayMessage::Applying { ssid } => format!("Connecting to {ssid}…"),
        DisplayMessage::Failed { ssid, reason } => {
            format!("Failed to join {ssid}: {reason}")
        }
        DisplayMessage::Connected { ssid } => format!("Connected to {ssid}. Rebooting…"),
    };
    let color = match message {
        DisplayMessage::Failed { .. } => ERROR_COLOR,
        DisplayMessage::Connected { .. } => ACCENT_COLOR,
        _ => TEXT_COLOR,
    };
    draw_text(
        &mut canvas,
        &font,
        PxScale::from(44.0),
        100,
        1040,
        color,
        &status_text,
    );

    Ok(pack_argb(canvas.into_raw()))
}

fn qr_to_rgba(img: &image::ImageBuffer<image::Luma<u8>, Vec<u8>>) -> RgbaImage {
    let mut out = RgbaImage::new(img.width(), img.height());
    for (x, y, pixel) in img.enumerate_pixels() {
        let v = pixel[0];
        let color = if v > 0 {
            [240, 244, 248, 255]
        } else {
            [10, 14, 18, 255]
        };
        out.put_pixel(x, y, Rgba(color));
    }
    out
}

fn pack_argb(raw: Vec<u8>) -> Vec<u32> {
    raw.chunks_exact(4)
        .map(|chunk| {
            let r = u32::from(chunk[0]);
            let g = u32::from(chunk[1]);
            let b = u32::from(chunk[2]);
            let a = u32::from(chunk[3]);
            (a << 24) | (r << 16) | (g << 8) | b
        })
        .collect()
}

fn draw_text(
    image: &mut RgbaImage,
    font: &FontArc,
    scale: PxScale,
    x: i32,
    y: i32,
    color: [u8; 4],
    text: &str,
) {
    let color = Rgba(color);
    let mut caret = point(x as f32, y as f32);
    let scaled_font = font.as_scaled(scale);
    let mut previous = None;
    for ch in text.chars() {
        let glyph_id = scaled_font.glyph_id(ch);
        if let Some(prev) = previous {
            caret.x += scaled_font.kern(prev, glyph_id);
        }
        let glyph = glyph_id.with_scale_and_position(scale, caret);
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            let origin_x = bounds.min.x.floor() as i32;
            let origin_y = bounds.min.y.floor() as i32;
            outlined.draw(|gx, gy, v| {
                let px = origin_x + gx as i32;
                let py = origin_y + gy as i32;
                if px < 0 || py < 0 {
                    return;
                }
                let px = px as u32;
                let py = py as u32;
                if px >= DISPLAY_WIDTH || py >= DISPLAY_HEIGHT {
                    return;
                }
                let alpha = (v * 255.0).round() as u8;
                let idx = ((py * DISPLAY_WIDTH + px) * 4) as usize;
                let dst = &mut image.as_mut()[idx..idx + 4];
                let inv = 255 - alpha;
                dst[0] =
                    ((dst[0] as u16 * inv as u16 + color[0] as u16 * alpha as u16) / 255) as u8;
                dst[1] =
                    ((dst[1] as u16 * inv as u16 + color[1] as u16 * alpha as u16) / 255) as u8;
                dst[2] =
                    ((dst[2] as u16 * inv as u16 + color[2] as u16 * alpha as u16) / 255) as u8;
                dst[3] = 255;
            });
        }
        caret.x += scaled_font.h_advance(glyph_id);
        previous = Some(glyph_id);
    }
}

pub async fn restart_application(cfg: &WifiSetupConfig) -> Result<()> {
    if cfg.restart_command.is_empty() {
        info!("restart command not configured; exiting process to trigger supervisor restart");
        std::process::exit(0);
    }
    run_command(&cfg.restart_command).await
}
