// The overlay renderer deliberately passes explicit geometry/color arguments
// between small drawing helpers to keep allocations out of the hot path.
#![allow(clippy::too_many_arguments)]

use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use anyhow::{anyhow, Context, Result};
use clap::Args;
use fontdb::{Database, Family, Query, Source};
use image::ImageReader;
use softbuffer::{Context as SoftContext, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalSize, Size};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Fullscreen, Window, WindowAttributes, WindowButtons, WindowId};

#[derive(Args, Debug)]
#[command(
    name = "overlay",
    about = "Show Wi-Fi recovery instructions in a kiosk overlay window."
)]
pub struct OverlayCli {
    /// Hotspot SSID to display in the overlay.
    #[arg(long)]
    pub ssid: String,
    /// Path to the hotspot password file.
    #[arg(long)]
    pub password_file: PathBuf,
    /// URL for the provisioning UI.
    #[arg(long)]
    pub ui_url: String,
    /// Optional headline override.
    #[arg(long)]
    pub title: Option<String>,
}

pub fn run(args: OverlayCli) -> Result<()> {
    let password = read_password(&args.password_file)?;
    let qr_asset = read_qr_asset_for_overlay(&args.password_file);
    let content = OverlayContent::new(args, password, qr_asset);
    let font = load_font()?;
    let event_loop = EventLoop::new()?;
    let mut app = OverlayApp::new(font, content);
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn with_overlay_app_id(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        use winit::platform::wayland::WindowAttributesExtWayland;
        return attrs.with_name("wifi-overlay", "wifi-overlay");
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    {
        attrs
    }
}

fn read_password(path: &Path) -> Result<String> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read hotspot password at {}", path.display()))?;
    let password = raw.trim().to_string();
    if password.is_empty() {
        Ok("(unavailable)".to_string())
    } else {
        Ok(password)
    }
}

#[derive(Clone)]
struct QrAsset {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl QrAsset {
    fn sample_luma(&self, sx: u32, sy: u32) -> u8 {
        let clamped_x = sx.min(self.width.saturating_sub(1));
        let clamped_y = sy.min(self.height.saturating_sub(1));
        let index = (clamped_y * self.width + clamped_x) as usize;
        self.pixels.get(index).copied().unwrap_or(255)
    }
}

fn read_qr_asset_for_overlay(password_file: &Path) -> Option<QrAsset> {
    let qr_path = password_file.parent()?.join("wifi-qr.png");
    let reader = ImageReader::open(&qr_path).ok()?;
    let image = reader.decode().ok()?;
    let luma = image.to_luma8();
    Some(QrAsset {
        width: luma.width(),
        height: luma.height(),
        pixels: luma.into_raw(),
    })
}

fn load_font() -> Result<FontArc> {
    let mut db = Database::new();
    db.load_system_fonts();

    let preferred_families = [
        Family::Name("IBM Plex Sans"),
        Family::Name("Inter"),
        Family::Name("Noto Sans"),
        Family::Name("DejaVu Sans"),
        Family::SansSerif,
    ];

    for family in preferred_families {
        if let Some(id) = db.query(&Query {
            families: &[family],
            ..Default::default()
        }) && let Some(font) = load_face(&db, id)?
        {
            return Ok(font);
        }
    }

    for face in db.faces() {
        if let Some(font) = load_face(&db, face.id)? {
            return Ok(font);
        }
    }

    Err(anyhow!("failed to load a system font for Wi-Fi overlay"))
}

fn load_face(db: &Database, id: fontdb::ID) -> Result<Option<FontArc>> {
    let face = db.face(id).context("missing font face in database")?;
    let font = match &face.source {
        Source::Binary(data) => {
            let bytes = data.as_ref().as_ref();
            let owned = bytes.to_vec();
            Some(
                FontArc::try_from_vec(owned)
                    .context("failed to decode font face from binary source")?,
            )
        }
        Source::File(path) => {
            let data = fs::read(path)
                .with_context(|| format!("failed to read font at {}", path.display()))?;
            Some(FontArc::try_from_vec(data).context("failed to decode font face from file data")?)
        }
        Source::SharedFile(_, data) => {
            let bytes = data.as_ref().as_ref();
            let owned = bytes.to_vec();
            Some(
                FontArc::try_from_vec(owned)
                    .context("failed to decode font face from shared file data")?,
            )
        }
    };
    Ok(font)
}

struct OverlayContent {
    title: String,
    subtitle: String,
    ssid: String,
    password: String,
    ui_url: String,
    footer: String,
    qr_asset: Option<QrAsset>,
}

impl OverlayContent {
    fn new(cli: OverlayCli, password: String, qr_asset: Option<QrAsset>) -> Self {
        let title = cli
            .title
            .unwrap_or_else(|| "Reconnect the photo frame to Wi-Fi".to_string());
        let subtitle = "Use another device to restore connectivity.".to_string();
        let footer = "The slideshow resumes automatically after the frame reconnects.".to_string();
        Self {
            title,
            subtitle,
            ssid: cli.ssid,
            password,
            ui_url: cli.ui_url,
            footer,
            qr_asset,
        }
    }
}

struct OverlayApp {
    window: Option<WindowHandle>,
    context: Option<SoftContext<WindowHandle>>,
    surface: Option<Surface<WindowHandle, WindowHandle>>,
    renderer: Renderer,
    needs_redraw: bool,
}

type WindowHandle = Arc<Window>;

impl OverlayApp {
    fn new(font: FontArc, content: OverlayContent) -> Self {
        Self {
            window: None,
            context: None,
            surface: None,
            renderer: Renderer::new(font, content),
            needs_redraw: true,
        }
    }

    fn ensure_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = with_overlay_app_id(
            Window::default_attributes()
                .with_title("Photo Frame Wi-Fi Recovery")
                .with_decorations(false)
                .with_resizable(false)
                .with_fullscreen(Some(Fullscreen::Borderless(None)))
                .with_active(true),
        );
        let window = event_loop
            .create_window(attrs)
            .expect("failed to create window");
        // Request fullscreen again after map so kiosk flows don't depend solely on sway rules.
        window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        window.set_cursor_visible(false);
        window.set_enabled_buttons(WindowButtons::empty());
        window.set_min_inner_size(Some(Size::Physical(PhysicalSize::new(640, 480))));
        let window = WindowHandle::new(window);

        let context =
            SoftContext::new(window.clone()).expect("failed to create softbuffer context");
        let surface =
            Surface::new(&context, window.clone()).expect("failed to create softbuffer surface");

        self.context = Some(context);
        self.surface = Some(surface);
        self.renderer.scale_factor = window.scale_factor() as f32;
        self.handle_resize(window.inner_size());
        self.window = Some(window);
        self.needs_redraw = true;
    }

    fn request_redraw(&mut self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_resize(&mut self, size: PhysicalSize<u32>) {
        if let Some(surface) = self.surface.as_mut()
            && let (Some(width), Some(height)) = (
                NonZeroU32::new(size.width.max(1)),
                NonZeroU32::new(size.height.max(1)),
            )
        {
            let _ = surface.resize(width, height);
            self.needs_redraw = true;
        }
    }

    fn handle_scale_change(&mut self, scale_factor: f64) {
        self.renderer.scale_factor = scale_factor as f32;
        self.needs_redraw = true;
    }

    fn render(&mut self) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let width = window.inner_size().width.max(1);
        let height = window.inner_size().height.max(1);
        if let Ok(mut buffer) = surface.buffer_mut() {
            let pixels = self.renderer.render(width, height);
            buffer.copy_from_slice(&pixels);
            if buffer.present().is_err() {
                eprintln!("failed to present overlay frame");
            }
        }
    }
}

impl ApplicationHandler for OverlayApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.ensure_window(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::Resized(size) => self.handle_resize(size),
            WindowEvent::RedrawRequested => {
                self.render();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.handle_scale_change(scale_factor);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.needs_redraw {
            self.request_redraw();
            self.needs_redraw = false;
        }
    }
}

struct Renderer {
    font: FontArc,
    content: OverlayContent,
    scale_factor: f32,
}

impl Renderer {
    fn new(font: FontArc, content: OverlayContent) -> Self {
        Self {
            font,
            content,
            scale_factor: 1.0,
        }
    }

    fn render(&self, width: u32, height: u32) -> Vec<u32> {
        let mut buffer = vec![0u32; (width as usize) * (height as usize)];
        let layout = OverlayLayout::for_surface(width, height, self.scale_factor);

        fill_vertical_gradient(
            &mut buffer,
            width,
            height,
            Color::from_rgb(0x070b12),
            Color::from_rgb(0x101924),
        );

        // Panel border glow + inner surface.
        draw_rounded_rect(
            &mut buffer,
            width,
            height,
            layout.panel_left,
            layout.panel_top,
            layout.panel_right,
            layout.panel_bottom,
            layout.panel_radius,
            Color::from_rgba(0x2e3f53, 0.86),
        );
        draw_rounded_rect(
            &mut buffer,
            width,
            height,
            layout.panel_left + layout.border_thickness,
            layout.panel_top + layout.border_thickness,
            layout.panel_right - layout.border_thickness,
            layout.panel_bottom - layout.border_thickness,
            (layout.panel_radius - layout.border_thickness).max(8.0),
            Color::from_rgba(0x0f1723, 0.96),
        );

        let mut cursor_y = layout.content_top;

        cursor_y = self.draw_title(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );
        cursor_y = self.draw_subtitle(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );
        cursor_y = self.draw_step_one(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );
        cursor_y = self.draw_step_two(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );
        cursor_y = self.draw_qr_section(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );
        cursor_y = self.draw_step_three(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );
        let _ = self.draw_footer(
            &mut buffer,
            width,
            height,
            cursor_y,
            layout.content_left,
            layout.content_width,
            layout.typography_scale,
        );

        buffer
    }

    fn draw_title(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        let size = (52.0 * scale).clamp(30.0, 92.0);
        let color = Color::from_rgb(0xf3f6fb);
        draw_paragraph(
            buffer,
            width,
            height,
            &self.font,
            &self.content.title,
            size,
            color,
            margin,
            top,
            max_width,
            (16.0 * scale).clamp(10.0, 32.0),
        ) + (20.0 * scale).clamp(10.0, 32.0)
    }

    fn draw_subtitle(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        let size = (25.0 * scale).clamp(17.0, 46.0);
        let color = Color::from_rgb(0xc8d2df);
        draw_paragraph(
            buffer,
            width,
            height,
            &self.font,
            &self.content.subtitle,
            size,
            color,
            margin,
            top,
            max_width,
            (14.0 * scale).clamp(8.0, 26.0),
        ) + (26.0 * scale).clamp(12.0, 40.0)
    }

    fn draw_step_one(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        let label = "1. Join the hotspot network:";
        let text = &self.content.ssid;
        draw_step_with_highlight(
            buffer, width, height, &self.font, label, text, top, margin, max_width, scale,
        )
    }

    fn draw_step_two(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        let label = "2. Enter the password:";
        let text = &self.content.password;
        draw_step_with_highlight(
            buffer, width, height, &self.font, label, text, top, margin, max_width, scale,
        )
    }

    fn draw_step_three(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        let label = "4. If needed, open this address manually:";
        let text = &self.content.ui_url;
        let step_top = draw_paragraph(
            buffer,
            width,
            height,
            &self.font,
            label,
            (23.0 * scale).clamp(16.0, 40.0),
            Color::from_rgb(0xe8eef7),
            margin,
            top,
            max_width,
            (10.0 * scale).clamp(6.0, 18.0),
        );
        draw_highlight(
            buffer,
            width,
            height,
            &self.font,
            text,
            (27.0 * scale).clamp(18.0, 50.0),
            margin,
            step_top,
            max_width,
            HighlightStyle::accent(scale),
            (20.0 * scale).clamp(12.0, 30.0),
        ) + (18.0 * scale).clamp(8.0, 30.0)
    }

    fn draw_qr_section(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        let label_top = draw_paragraph(
            buffer,
            width,
            height,
            &self.font,
            "3. Scan this QR code to open setup on your phone:",
            (23.0 * scale).clamp(16.0, 40.0),
            Color::from_rgb(0xe8eef7),
            margin,
            top,
            max_width,
            (10.0 * scale).clamp(6.0, 18.0),
        );

        let qr_side = (250.0 * scale).clamp(170.0, 420.0).min(max_width);
        let card_pad = (14.0 * scale).clamp(8.0, 24.0);
        let card_side = qr_side + 2.0 * card_pad;
        let card_left = margin;
        let card_top = label_top;

        draw_rounded_rect(
            buffer,
            width,
            height,
            card_left,
            card_top,
            card_left + card_side,
            card_top + card_side,
            (16.0 * scale).clamp(10.0, 24.0),
            Color::from_rgb(0xffffff),
        );

        if let Some(qr_asset) = &self.content.qr_asset {
            draw_qr_asset(
                buffer,
                width,
                height,
                qr_asset,
                card_left + card_pad,
                card_top + card_pad,
                qr_side,
            );
        } else {
            let fallback = "QR unavailable";
            let text_scale = PxScale::from((18.0 * scale).clamp(12.0, 30.0));
            let text_width = measure_text(fallback, &self.font, text_scale);
            let text_left = card_left + ((card_side - text_width) * 0.5).max(card_pad);
            let baseline = card_top + card_side * 0.55;
            draw_text(
                buffer,
                width,
                height,
                &self.font,
                fallback,
                Color::from_rgb(0x334155),
                text_left,
                baseline,
                text_scale,
            );
        }

        card_top + card_side + (18.0 * scale).clamp(8.0, 30.0)
    }

    fn draw_footer(
        &self,
        buffer: &mut [u32],
        width: u32,
        height: u32,
        top: f32,
        margin: f32,
        max_width: f32,
        scale: f32,
    ) -> f32 {
        draw_paragraph(
            buffer,
            width,
            height,
            &self.font,
            &self.content.footer,
            (21.0 * scale).clamp(14.0, 38.0),
            Color::from_rgb(0xa6b2c1),
            margin,
            top,
            max_width,
            (12.0 * scale).clamp(7.0, 22.0),
        )
    }
}

struct OverlayLayout {
    panel_left: f32,
    panel_top: f32,
    panel_right: f32,
    panel_bottom: f32,
    panel_radius: f32,
    border_thickness: f32,
    content_left: f32,
    content_top: f32,
    content_width: f32,
    typography_scale: f32,
}

impl OverlayLayout {
    fn for_surface(width: u32, height: u32, scale_factor: f32) -> Self {
        let width_f = width.max(1) as f32;
        let height_f = height.max(1) as f32;
        let viewport_scale = (width_f / 1920.0).min(height_f / 1080.0).clamp(0.85, 2.2);
        let scale = (viewport_scale * scale_factor.clamp(1.0, 1.2)).clamp(0.85, 2.4);

        let outer_margin = (28.0 * scale).clamp(18.0, 76.0);
        let available_width = (width_f - 2.0 * outer_margin).max(320.0);
        let panel_target_width = (1480.0 * viewport_scale).clamp(740.0, available_width);
        let panel_width = panel_target_width.min(available_width);
        let panel_left = ((width_f - panel_width) * 0.5).max(outer_margin);
        let panel_right = panel_left + panel_width;
        let panel_top = outer_margin;
        let panel_bottom = (height_f - outer_margin).max(panel_top + 200.0);

        let panel_padding = (50.0 * scale).clamp(24.0, 94.0);
        let content_left = panel_left + panel_padding;
        let content_top = panel_top + panel_padding;
        let content_width = (panel_width - panel_padding * 2.0).max(260.0);

        Self {
            panel_left,
            panel_top,
            panel_right,
            panel_bottom,
            panel_radius: (34.0 * scale).clamp(16.0, 52.0),
            border_thickness: (3.0 * scale).clamp(2.0, 6.0),
            content_left,
            content_top,
            content_width,
            typography_scale: scale,
        }
    }
}

fn draw_step_with_highlight(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    font: &FontArc,
    label: &str,
    value: &str,
    top: f32,
    margin: f32,
    max_width: f32,
    scale: f32,
) -> f32 {
    let step_top = draw_paragraph(
        buffer,
        width,
        height,
        font,
        label,
        (23.0 * scale).clamp(16.0, 40.0),
        Color::from_rgb(0xe8eef7),
        margin,
        top,
        max_width,
        (10.0 * scale).clamp(6.0, 18.0),
    );
    draw_highlight(
        buffer,
        width,
        height,
        font,
        value,
        (29.0 * scale).clamp(20.0, 54.0),
        margin,
        step_top,
        max_width,
        HighlightStyle::primary(scale),
        (20.0 * scale).clamp(12.0, 30.0),
    ) + (16.0 * scale).clamp(8.0, 30.0)
}

#[derive(Clone, Copy)]
struct LineMetrics {
    ascent: f32,
    descent: f32,
    line_gap: f32,
}

fn draw_paragraph(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    font: &FontArc,
    text: &str,
    size: f32,
    color: Color,
    left: f32,
    top: f32,
    max_width: f32,
    line_gap: f32,
) -> f32 {
    let scale = PxScale::from(size);
    let metrics = line_metrics(font, scale);

    let mut cursor_y = top + metrics.ascent;
    for line in wrap_text(text, font, scale, max_width) {
        draw_text(
            buffer, width, height, font, &line, color, left, cursor_y, scale,
        );
        cursor_y += metrics.descent + metrics.line_gap + line_gap;
    }
    cursor_y
}

fn draw_highlight(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    font: &FontArc,
    text: &str,
    size: f32,
    left: f32,
    top: f32,
    max_width: f32,
    style: HighlightStyle,
    corner_radius: f32,
) -> f32 {
    let scale = PxScale::from(size);
    let metrics = line_metrics(font, scale);
    let text_max_width = (max_width - 2.0 * style.pad_x).max(80.0);
    let lines = wrap_text(text, font, scale, text_max_width);

    let mut cursor_y = top;
    for line in lines.iter() {
        let text_width = measure_text(line, font, scale);
        let ideal_width = text_width + 2.0 * style.pad_x;
        let min_width = (max_width * style.min_width_ratio).max(ideal_width);
        let background_width = min_width.min(max_width);
        let background_height = metrics.ascent + metrics.descent + 2.0 * style.pad_y;
        let background_left = left;
        let background_top = cursor_y;
        draw_rounded_rect(
            buffer,
            width,
            height,
            background_left,
            background_top,
            background_left + background_width,
            background_top + background_height,
            corner_radius,
            style.background,
        );
        draw_text(
            buffer,
            width,
            height,
            font,
            line,
            style.foreground,
            left + style.pad_x,
            cursor_y + style.pad_y + metrics.ascent,
            scale,
        );
        cursor_y += background_height + metrics.line_gap;
    }

    cursor_y
}

fn draw_qr_asset(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    qr: &QrAsset,
    left: f32,
    top: f32,
    side: f32,
) {
    if qr.width == 0 || qr.height == 0 || side <= 1.0 {
        return;
    }
    let x0 = left.max(0.0).floor() as i32;
    let y0 = top.max(0.0).floor() as i32;
    let x1 = (left + side).min(width as f32).ceil() as i32;
    let y1 = (top + side).min(height as f32).ceil() as i32;
    let draw_w = (x1 - x0).max(1) as u32;
    let draw_h = (y1 - y0).max(1) as u32;

    for dy in 0..draw_h {
        for dx in 0..draw_w {
            let sx = ((dx as f32 / draw_w as f32) * qr.width as f32).floor() as u32;
            let sy = ((dy as f32 / draw_h as f32) * qr.height as f32).floor() as u32;
            let luma = qr.sample_luma(sx, sy);
            let color = if luma < 128 {
                Color::from_rgb(0x111111)
            } else {
                Color::from_rgb(0xffffff)
            };
            blend_pixel(
                buffer,
                width,
                height,
                (x0 as u32 + dx) as f32,
                (y0 as u32 + dy) as f32,
                color,
                1.0,
            );
        }
    }
}

fn wrap_text(text: &str, font: &FontArc, scale: PxScale, max_width: f32) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in words {
        let candidate = if current_line.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", current_line, word)
        };

        if measure_text(&candidate, font, scale) <= max_width {
            current_line = candidate;
        } else {
            if !current_line.is_empty() {
                lines.push(current_line.clone());
            }
            current_line = word.to_string();
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

fn line_metrics(font: &FontArc, scale: PxScale) -> LineMetrics {
    let scaled = font.as_scaled(scale);
    LineMetrics {
        ascent: scaled.ascent(),
        descent: scaled.descent().abs(),
        line_gap: scaled.line_gap(),
    }
}

fn draw_text(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    font: &FontArc,
    text: &str,
    color: Color,
    left: f32,
    baseline: f32,
    scale: PxScale,
) {
    let scaled = font.as_scaled(scale);
    let mut cursor_x = left;
    let mut previous = None;
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        let glyph = scaled.glyph_id(ch);
        if let Some(prev) = previous {
            cursor_x += scaled.kern(prev, glyph);
        }
        let advance = scaled.h_advance(glyph);
        let mut positioned = scaled.scaled_glyph(ch);
        positioned.position = point(cursor_x, baseline);
        if let Some(outline) = font.outline_glyph(positioned) {
            let bounds = outline.px_bounds();
            outline.draw(|x, y, coverage| {
                blend_pixel(
                    buffer,
                    width,
                    height,
                    bounds.min.x + x as f32,
                    bounds.min.y + y as f32,
                    color,
                    coverage,
                );
            });
        }
        cursor_x += advance;
        previous = Some(glyph);
    }
}

fn draw_rounded_rect(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
    color: Color,
) {
    let rect_width = right - left;
    let rect_height = bottom - top;

    if radius <= 0.0 || radius * 2.0 >= rect_width.min(rect_height) {
        fill_rect(buffer, width, height, left, top, right, bottom, color);
        return;
    }

    fill_rect(
        buffer,
        width,
        height,
        left + radius,
        top,
        right - radius,
        bottom,
        color,
    );
    fill_rect(
        buffer,
        width,
        height,
        left,
        top + radius,
        left + radius,
        bottom - radius,
        color,
    );
    fill_rect(
        buffer,
        width,
        height,
        right - radius,
        top + radius,
        right,
        bottom - radius,
        color,
    );

    draw_corner(
        buffer,
        width,
        height,
        left + radius,
        top + radius,
        radius,
        color,
        Corner::TopLeft,
    );
    draw_corner(
        buffer,
        width,
        height,
        right - radius,
        top + radius,
        radius,
        color,
        Corner::TopRight,
    );
    draw_corner(
        buffer,
        width,
        height,
        left + radius,
        bottom - radius,
        radius,
        color,
        Corner::BottomLeft,
    );
    draw_corner(
        buffer,
        width,
        height,
        right - radius,
        bottom - radius,
        radius,
        color,
        Corner::BottomRight,
    );
}

#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

fn draw_corner(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    radius: f32,
    color: Color,
    corner: Corner,
) {
    let radius_i = radius.ceil() as i32;
    for dy in -radius_i..=radius_i {
        for dx in -radius_i..=radius_i {
            let x = center_x + dx as f32;
            let y = center_y + dy as f32;
            if point_in_corner(dx as f32, dy as f32, radius, corner) {
                blend_pixel(buffer, width, height, x, y, color, 1.0);
            }
        }
    }
}

fn point_in_corner(dx: f32, dy: f32, radius: f32, corner: Corner) -> bool {
    let distance = (dx * dx + dy * dy).sqrt();
    if distance > radius {
        return false;
    }
    match corner {
        Corner::TopLeft => dx <= 0.0 && dy <= 0.0,
        Corner::TopRight => dx >= 0.0 && dy <= 0.0,
        Corner::BottomLeft => dx <= 0.0 && dy >= 0.0,
        Corner::BottomRight => dx >= 0.0 && dy >= 0.0,
    }
}

fn measure_text(text: &str, font: &FontArc, scale: PxScale) -> f32 {
    let scaled_font = font.as_scaled(scale);
    let mut width = 0.0f32;
    let mut previous = None;
    for ch in text.chars() {
        if ch == '\n' {
            continue;
        }
        let glyph_id = scaled_font.glyph_id(ch);
        if let Some(prev) = previous {
            width += scaled_font.kern(prev, glyph_id);
        }
        width += scaled_font.h_advance(glyph_id);
        previous = Some(glyph_id);
    }
    width.max(0.0)
}

fn fill_rect(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    color: Color,
) {
    let x0 = left.max(0.0).floor() as i32;
    let y0 = top.max(0.0).floor() as i32;
    let x1 = right.min(width as f32).ceil() as i32;
    let y1 = bottom.min(height as f32).ceil() as i32;
    for y in y0.max(0)..y1.min(height as i32) {
        for x in x0.max(0)..x1.min(width as i32) {
            blend_pixel(buffer, width, height, x as f32, y as f32, color, color.a);
        }
    }
}

fn fill_vertical_gradient(buffer: &mut [u32], width: u32, height: u32, top: Color, bottom: Color) {
    if width == 0 || height == 0 {
        return;
    }
    for y in 0..height {
        let t = if height <= 1 {
            0.0
        } else {
            y as f32 / (height - 1) as f32
        };
        let row = Color {
            r: top.r + (bottom.r - top.r) * t,
            g: top.g + (bottom.g - top.g) * t,
            b: top.b + (bottom.b - top.b) * t,
            a: top.a + (bottom.a - top.a) * t,
        };
        let packed = pack_color(row.rgb());
        let start = (y * width) as usize;
        let end = start + width as usize;
        for pixel in &mut buffer[start..end] {
            *pixel = packed;
        }
    }
}

fn blend_pixel(
    buffer: &mut [u32],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    color: Color,
    coverage: f32,
) {
    if coverage <= 0.0 {
        return;
    }
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    if xi < 0 || yi < 0 || xi >= width as i32 || yi >= height as i32 {
        return;
    }
    let idx = (yi as u32 * width + xi as u32) as usize;
    let src_a = (color.a * coverage).clamp(0.0, 1.0);
    let src = color.rgb();
    let dst = unpack_color(buffer[idx]);
    let out = blend(src, dst, src_a);
    buffer[idx] = pack_color(out);
}

fn blend(src: (f32, f32, f32), dst: (f32, f32, f32), alpha: f32) -> (f32, f32, f32) {
    (
        src.0 * alpha + dst.0 * (1.0 - alpha),
        src.1 * alpha + dst.1 * (1.0 - alpha),
        src.2 * alpha + dst.2 * (1.0 - alpha),
    )
}

fn unpack_color(value: u32) -> (f32, f32, f32) {
    let r = ((value >> 16) & 0xFF) as f32 / 255.0;
    let g = ((value >> 8) & 0xFF) as f32 / 255.0;
    let b = (value & 0xFF) as f32 / 255.0;
    (r, g, b)
}

fn pack_color(color: (f32, f32, f32)) -> u32 {
    let r = (color.0.clamp(0.0, 1.0) * 255.0).round() as u32;
    let g = (color.1.clamp(0.0, 1.0) * 255.0).round() as u32;
    let b = (color.2.clamp(0.0, 1.0) * 255.0).round() as u32;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

#[derive(Clone, Copy)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Color {
    fn from_rgb(hex: u32) -> Self {
        let r = ((hex >> 16) & 0xFF) as f32 / 255.0;
        let g = ((hex >> 8) & 0xFF) as f32 / 255.0;
        let b = (hex & 0xFF) as f32 / 255.0;
        Self { r, g, b, a: 1.0 }
    }

    fn from_rgba(hex: u32, alpha: f32) -> Self {
        let mut value = Self::from_rgb(hex);
        value.a = alpha.clamp(0.0, 1.0);
        value
    }

    fn rgb(self) -> (f32, f32, f32) {
        (self.r, self.g, self.b)
    }
}

struct HighlightStyle {
    background: Color,
    foreground: Color,
    pad_x: f32,
    pad_y: f32,
    min_width_ratio: f32,
}

impl HighlightStyle {
    fn primary(scale: f32) -> Self {
        Self {
            background: Color::from_rgb(0x2458bc),
            foreground: Color::from_rgb(0xffffff),
            pad_x: (22.0 * scale).clamp(12.0, 38.0),
            pad_y: (15.0 * scale).clamp(8.0, 26.0),
            min_width_ratio: 0.58,
        }
    }

    fn accent(scale: f32) -> Self {
        Self {
            background: Color::from_rgb(0x1f8f66),
            foreground: Color::from_rgb(0xffffff),
            pad_x: (22.0 * scale).clamp(12.0, 38.0),
            pad_y: (15.0 * scale).clamp(8.0, 26.0),
            min_width_ratio: 0.72,
        }
    }
}
