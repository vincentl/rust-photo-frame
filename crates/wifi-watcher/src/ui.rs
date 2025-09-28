use std::env;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use eframe::{egui, App};
use egui::{ColorImage, TextureHandle, TextureOptions};
use qrcode::QrCode;
use tracing::{info, warn};
use users::get_user_by_name;
use users::os::unix::UserExt;

use crate::check::Settings;
use crate::hotspot::HotspotInfo;

pub struct UiHandle {
    child: Option<Child>,
}

impl UiHandle {
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            if let Err(err) = terminate_child(&mut child) {
                warn!(?err, "failed to stop Wi-Fi UI cleanly");
            }
        }
    }
}

impl Drop for UiHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn spawn(settings: &Settings, info: HotspotInfo) -> Result<UiHandle> {
    let mut command = Command::new(env::current_exe().context("resolving wifi-watcher executable")?);
    command.arg("--ui");
    command.env("HOTSPOT_SSID", &info.ssid);
    command.env("HOTSPOT_PASSWORD", &info.password);
    command.env("HOTSPOT_IP", &info.ip);

    configure_for_frame_user(&mut command, &settings.frame_user)?;

    let child = command
        .spawn()
        .context("spawning Wi-Fi provisioning UI as frame user")?;
    Ok(UiHandle {
        child: Some(child),
    })
}

pub fn run_from_env() -> Result<()> {
    let info = HotspotInfo {
        ssid: env::var("HOTSPOT_SSID").context("HOTSPOT_SSID not set")?,
        password: env::var("HOTSPOT_PASSWORD").context("HOTSPOT_PASSWORD not set")?,
        ip: env::var("HOTSPOT_IP").context("HOTSPOT_IP not set")?,
    };
    run_ui(info)
}

fn configure_for_frame_user(command: &mut Command, frame_user: &str) -> Result<()> {
    let user = get_user_by_name(frame_user)
        .ok_or_else(|| anyhow!("frame user '{frame_user}' not found"))?;

    let uid = user.uid();
    let gid = user.primary_group_id();
    let username = user.name().to_string_lossy().to_string();
    let home = user.home_dir().to_path_buf();

    command.uid(uid);
    command.gid(gid);
    command.current_dir(&home);
    command.env("HOME", &home);
    command.env("USER", &username);
    command.env("LOGNAME", &username);

    if let Ok(rust_log) = env::var("RUST_LOG") {
        command.env("RUST_LOG", rust_log);
    }

    let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
    if runtime_dir.exists() {
        command.env("XDG_RUNTIME_DIR", runtime_dir);
    }

    if let Ok(display) = env::var("FRAME_DISPLAY") {
        command.env("DISPLAY", display);
    } else if let Ok(display) = env::var("DISPLAY") {
        command.env("DISPLAY", display);
    } else {
        command.env("DISPLAY", ":0");
    }

    if let Ok(wayland_display) = env::var("FRAME_WAYLAND_DISPLAY") {
        command.env("WAYLAND_DISPLAY", wayland_display);
    }

    let xauthority = home.join(".Xauthority");
    if xauthority.exists() {
        command.env("XAUTHORITY", xauthority);
    }

    Ok(())
}

fn terminate_child(child: &mut Child) -> Result<()> {
    let pid = child.id();
    if pid == 0 {
        return Ok(());
    }

    unsafe {
        if libc::kill(pid as i32, libc::SIGTERM) != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::InvalidInput && err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err).context("sending SIGTERM to Wi-Fi UI");
            }
        }
    }

    for _ in 0..10 {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    child.kill().context("forcing Wi-Fi UI shutdown")?;
    let _ = child.wait();
    Ok(())
}

fn run_ui(info: HotspotInfo) -> Result<()> {
    let title = "Wi-Fi Down".to_string();
    let viewport = egui::ViewportBuilder::default()
        .with_title(title.clone())
        .with_decorations(false)
        .with_fullscreen(true)
        .with_maximized(true);
    let native_options = eframe::NativeOptions {
        viewport,
        follow_system_theme: true,
        ..Default::default()
    };

    info!(ssid = %info.ssid, "launching Wi-Fi down UI");
    eframe::run_native(
        &title,
        native_options,
        Box::new(move |cc| Ok(Box::new(WifiDownApp::new(cc.egui_ctx.clone(), info.clone())))),
    )
    .context("running Wi-Fi down UI")?;
    Ok(())
}

struct WifiDownApp {
    ctx: egui::Context,
    info: HotspotInfo,
    texture: Option<TextureHandle>,
}

impl WifiDownApp {
    fn new(ctx: egui::Context, info: HotspotInfo) -> Self {
        Self {
            ctx,
            info,
            texture: None,
        }
    }

    fn texture(&mut self) -> Option<&TextureHandle> {
        if self.texture.is_none() {
            if let Ok(image) = make_qr(&self.info) {
                let tex = self
                    .ctx
                    .load_texture("wifi-down-qr", image, TextureOptions::NEAREST);
                self.texture = Some(tex);
            }
        }
        self.texture.as_ref()
    }
}

impl App for WifiDownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.heading("Wi-Fi Connection is Down");
                ui.add_space(12.0);
                ui.label(format!(
                    "Connect to '{}' and open http://{}/",
                    self.info.ssid, self.info.ip
                ));
                ui.add_space(12.0);
                if let Some(tex) = self.texture() {
                    let size = ui.available_width().min(320.0);
                    ui.image((tex.id(), egui::Vec2::splat(size)));
                }
                ui.add_space(16.0);
                ui.label(format!("Hotspot password: {}", self.info.password));
                ui.add_space(12.0);
                ui.label("Leave this window open while you update Wi-Fi settings.");
            });
        });

        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn make_qr(info: &HotspotInfo) -> Result<ColorImage> {
    let url = format!("http://{}/", info.ip);
    let code = QrCode::new(url.as_bytes()).context("building QR code")?;
    let matrix = code.to_colors();
    let dimension = code.width();
    let mut pixels = Vec::with_capacity(dimension * dimension * 4);
    for color in matrix {
        let value = if color == qrcode::Color::Dark { 0 } else { 255 };
        pixels.extend_from_slice(&[value, value, value, 255]);
    }
    Ok(ColorImage::from_rgba_unmultiplied(
        [dimension, dimension],
        &pixels,
    ))
}
