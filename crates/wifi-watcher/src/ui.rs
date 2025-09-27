use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use anyhow::{Context, Result};
use eframe::{egui, App};
use egui::{ColorImage, TextureHandle, TextureOptions, ViewportCommand};
use qrcode::QrCode;
use tracing::{error, info};

use crate::hotspot::HotspotInfo;

pub struct UiHandle {
    stop_tx: Option<Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

impl UiHandle {
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for UiHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn spawn(info: HotspotInfo) -> Result<UiHandle> {
    let (tx, rx) = mpsc::channel();
    let join = thread::spawn(move || {
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
        let mut stop_rx = Some(rx);
        if let Err(err) = eframe::run_native(
            &title,
            native_options,
            Box::new(move |cc| {
                let rx = stop_rx
                    .take()
                    .expect("WiFi down UI should be constructed once");
                Ok(Box::new(WifiDownApp::new(
                    cc.egui_ctx.clone(),
                    info.clone(),
                    rx,
                )))
            }),
        ) {
            error!(?err, "wifi down UI failed");
        }
    });
    Ok(UiHandle {
        stop_tx: Some(tx),
        join: Some(join),
    })
}

struct WifiDownApp {
    ctx: egui::Context,
    info: HotspotInfo,
    stop_rx: Receiver<()>,
    texture: Option<TextureHandle>,
}

impl WifiDownApp {
    fn new(ctx: egui::Context, info: HotspotInfo, stop_rx: Receiver<()>) -> Self {
        Self {
            ctx,
            info,
            stop_rx,
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
        match self.stop_rx.try_recv() {
            Ok(_) | Err(TryRecvError::Disconnected) => {
                ctx.send_viewport_cmd(ViewportCommand::Close);
                return;
            }
            Err(TryRecvError::Empty) => {}
        }

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
