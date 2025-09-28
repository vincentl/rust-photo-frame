use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Child, Command};

use anyhow::{anyhow, Context, Result};
use eframe::{egui, App};
use egui::{ColorImage, TextureHandle, TextureOptions};
use qrcode::QrCode;
use tracing::{info, warn};

use crate::check::Settings;
use crate::hotspot::HotspotInfo;

pub struct UiHandle {
    child: Option<Child>,
}

impl UiHandle {
    pub fn is_alive(&mut self) -> Result<bool> {
        if let Some(mut child) = self.child.take() {
            let result = match child.try_wait() {
                Ok(Some(_status)) => Ok(false),
                Ok(None) => {
                    self.child = Some(child);
                    Ok(true)
                }
                Err(err) => {
                    warn!(?err, "failed to poll wifi UI child status");
                    self.child = Some(child);
                    Err(err.into())
                }
            };
            result
        } else {
            Ok(false)
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.kill() {
                Ok(()) => {
                    let _ = child.wait();
                }
                Err(err) if err.kind() == ErrorKind::InvalidInput => {}
                Err(err) => {
                    warn!(?err, "failed to terminate wifi UI process");
                }
            }
        }
    }
}

impl Drop for UiHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn spawn(info: &HotspotInfo, settings: &Settings) -> Result<UiHandle> {
    let session = SessionEnv::from_settings(settings)?;
    let exe = std::env::current_exe().context("locating wifi-watcher executable")?;
    let payload = serde_json::to_string(info).context("serializing hotspot info for UI")?;

    let child = if session.use_sudo {
        let mut command = Command::new("sudo");
        command.arg("-u").arg(&session.user);
        command.arg("env");
        for pair in session.env_pairs() {
            command.arg(pair);
        }
        command.arg(format!("WIFI_UI_PAYLOAD={payload}"));
        command.arg(&exe);
        command.arg("--show-ui");
        command
            .spawn()
            .context("launching wifi UI process with sudo")?
    } else {
        let mut command = Command::new(&exe);
        session.apply_direct_env(&mut command);
        command.arg("--show-ui");
        command
            .env("WIFI_UI_PAYLOAD", payload)
            .spawn()
            .context("launching wifi UI process")?
    };

    Ok(UiHandle {
        child: Some(child),
    })
}

pub fn run_blocking(info: HotspotInfo) -> Result<()> {
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
    info!("launching Wi-Fi down UI");
    eframe::run_native(
        &title,
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(WifiDownApp::new(
                cc.egui_ctx.clone(),
                info.clone(),
            )))
        }),
    )
    .map_err(|err| anyhow!(err))?;
    Ok(())
}

struct SessionEnv {
    user: String,
    use_sudo: bool,
    env: Vec<(String, String)>,
}

impl SessionEnv {
    fn from_settings(settings: &Settings) -> Result<Self> {
        let user = settings.frame_user.clone();
        let use_sudo = user != "root";
        let record = lookup_user(&user)?;

        let mut env = Vec::new();

        if let Ok(value) = std::env::var("WAYLAND_DISPLAY") {
            env.push(("WAYLAND_DISPLAY".into(), value));
        }

        let display = std::env::var("DISPLAY")
            .or_else(|_| std::env::var("FRAME_DISPLAY"))
            .unwrap_or_else(|_| ":0".to_string());
        env.push(("DISPLAY".into(), display));

        if let Ok(value) = std::env::var("XAUTHORITY") {
            env.push(("XAUTHORITY".into(), value));
        } else if let Some(home) = record.home.as_ref() {
            let path = home.join(".Xauthority");
            env.push(("XAUTHORITY".into(), path.to_string_lossy().into_owned()));
        }

        if let Ok(value) = std::env::var("XDG_RUNTIME_DIR") {
            env.push(("XDG_RUNTIME_DIR".into(), value.clone()));
            if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_err() {
                env.push((
                    "DBUS_SESSION_BUS_ADDRESS".into(),
                    format!("unix:path={}/bus", value),
                ));
            }
        } else if let Some(uid) = record.uid {
            let runtime = format!("/run/user/{uid}");
            env.push(("XDG_RUNTIME_DIR".into(), runtime.clone()));
            env.push((
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={runtime}/bus"),
            ));
        } else if use_sudo {
            return Err(anyhow!(
                "unable to determine runtime directory for user '{}'",
                user
            ));
        }

        Ok(Self {
            user,
            use_sudo,
            env,
        })
    }

    fn env_pairs(&self) -> impl Iterator<Item = String> + '_ {
        self.env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
    }

    fn apply_direct_env(&self, command: &mut Command) {
        for (key, value) in &self.env {
            command.env(key, value);
        }
    }
}

struct UserRecord {
    uid: Option<u32>,
    home: Option<PathBuf>,
}

fn lookup_user(username: &str) -> Result<UserRecord> {
    let contents = fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    for line in contents.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split(':');
        let name = parts.next().unwrap_or("");
        if name != username {
            continue;
        }
        let _password = parts.next();
        let uid = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok());
        let _gid = parts.next();
        let _gecos = parts.next();
        let home = parts.next().map(PathBuf::from);
        return Ok(UserRecord { uid, home });
    }
    if username == "root" {
        return Ok(UserRecord {
            uid: Some(0),
            home: Some(PathBuf::from("/root")),
        });
    }
    Err(anyhow!("user '{username}' not found in /etc/passwd"))
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

        ctx.request_repaint_after(std::time::Duration::from_millis(200));
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
