use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use anyhow::{Context, Result, anyhow};
use clap::Args;
use fontdb::{Database, Family, Query, Source};
use softbuffer::{Context as SoftContext, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalSize, Size};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowButtons, WindowId};

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
    let content = OverlayContent::new(args, password);
    let font = load_font()?;
    let event_loop = EventLoop::new()?;
    let mut app = OverlayApp::new(font, content);
    event_loop.run_app(&mut app)?;
    Ok(())
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
        }) {
            if let Some(font) = load_face(&db, id)? {
                return Ok(font);
            }
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
}

impl OverlayContent {
    fn new(cli: OverlayCli, password: String) -> Self {
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

        let attrs = Window::default_attributes()
            .with_title("Photo Frame Wi-Fi Recovery")
            .with_decorations(false)
            .with_resizable(true)
            .with_active(true);
        let window = event_loop
            .create_window(attrs)
            .expect("failed to create window");
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
        self.window = Some(window);
        self.needs_redraw = true;
    }

    fn request_redraw(&mut self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_resize(&mut self, size: PhysicalSize<u32>) {
        if let Some(surface) = self.surface.as_mut() {
            if let (Some(width), Some(height)) = (
                NonZeroU32::new(size.width.max(1)),
                NonZeroU32::new(size.height.max(1)),
            ) {
                let _ = surface.resize(width, height);
                self.needs_redraw = true;
            }
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
        let bg = Color::from_rgb(0x0b1014);
        fill_rect(
            &mut buffer,
            width,
            height,
            0.0,
            0.0,
            width as f32,
            height as f32,
            bg,
        );

        let scale = self.scale_factor.max(1.0);
        let margin = 80.0 * scale;
        let max_width = (width as f32 - 2.0 * margin).max(320.0 * scale);

        let mut cursor_y = margin;

        cursor_y = self.draw_title(
            &mut buffer,
            width,
            height,
            cursor_y,
            margin,
            max_width,
            scale,
        );
        cursor_y = self.draw_subtitle(
            &mut buffer,
            width,
            height,
            cursor_y,
            margin,
            max_width,
            scale,
        );
        cursor_y = self.draw_step_one(
            &mut buffer,
            width,
            height,
            cursor_y,
            margin,
            max_width,
            scale,
        );
        cursor_y = self.draw_step_two(
            &mut buffer,
            width,
            height,
            cursor_y,
            margin,
            max_width,
            scale,
        );
        cursor_y = self.draw_step_three(
            &mut buffer,
            width,
            height,
            cursor_y,
            margin,
            max_width,
            scale,
        );
        let _ = self.draw_footer(
            &mut buffer,
            width,
            height,
            cursor_y,
            margin,
            max_width,
            scale,
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
        let size = 56.0 * scale;
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
            28.0 * scale,
        )
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
        let size = 30.0 * scale;
        let color = Color::from_rgb(0xc7ccd7);
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
            38.0 * scale,
        )
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
        let label = "3. Visit this address and follow the prompts:";
        let text = &self.content.ui_url;
        let step_top = draw_paragraph(
            buffer,
            width,
            height,
            &self.font,
            label,
            30.0 * scale,
            Color::from_rgb(0xf3f6fb),
            margin,
            top,
            max_width,
            20.0 * scale,
        );
        draw_highlight(
            buffer,
            width,
            height,
            &self.font,
            text,
            32.0 * scale,
            margin,
            step_top,
            max_width,
            HighlightStyle::accent(scale),
            48.0 * scale,
        )
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
            26.0 * scale,
            Color::from_rgb(0x98a1ae),
            margin,
            top,
            max_width,
            28.0 * scale,
        )
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
        30.0 * scale,
        Color::from_rgb(0xf3f6fb),
        margin,
        top,
        max_width,
        20.0 * scale,
    );
    draw_highlight(
        buffer,
        width,
        height,
        font,
        value,
        34.0 * scale,
        margin,
        step_top,
        max_width,
        HighlightStyle::primary(scale),
        48.0 * scale,
    )
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
    let lines = wrap_text(text, font, scale, max_width - 2.0 * style.pad_x);

    let mut cursor_y = top;
    for line in lines.iter() {
        let text_width = measure_text(line, font, scale);
        let background_width = text_width + 2.0 * style.pad_x;
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

fn wrap_text<'a>(text: &'a str, font: &FontArc, scale: PxScale, max_width: f32) -> Vec<String> {
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
    for ch in text.chars() {
        if ch.is_control() {
            continue;
        }
        let glyph = scaled.glyph_id(ch);
        let advance = scaled.h_advance(glyph);
        if let Some(outline) = font.outline_glyph(scaled.scaled_glyph(ch)) {
            outline.draw(|x, y, coverage| {
                let fx = x as f32;
                let fy = y as f32;
                blend_pixel(
                    buffer,
                    width,
                    height,
                    cursor_x + fx,
                    baseline - scaled.ascent() + fy,
                    color,
                    coverage,
                );
            });
        }
        cursor_x += advance;
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
    for ch in text.chars() {
        if ch == '\n' {
            continue;
        }
        let glyph_id = scaled_font.glyph_id(ch);
        width += scaled_font.h_advance(glyph_id);
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

    fn rgb(self) -> (f32, f32, f32) {
        (self.r, self.g, self.b)
    }
}

struct HighlightStyle {
    background: Color,
    foreground: Color,
    pad_x: f32,
    pad_y: f32,
}

impl HighlightStyle {
    fn primary(scale: f32) -> Self {
        Self {
            background: Color::from_rgb(0x1c61d6),
            foreground: Color::from_rgb(0xffffff),
            pad_x: 28.0 * scale,
            pad_y: 22.0 * scale,
        }
    }

    fn accent(scale: f32) -> Self {
        Self {
            background: Color::from_rgb(0x1a8f67),
            foreground: Color::from_rgb(0xffffff),
            pad_x: 28.0 * scale,
            pad_y: 22.0 * scale,
        }
    }
}
