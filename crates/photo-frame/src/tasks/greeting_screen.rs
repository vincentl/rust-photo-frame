use std::path::PathBuf;
use std::str::FromStr;

use fontdb::{Database, Family, Query};
use glyphon::cosmic_text::Align;
use glyphon::{
    Attrs, Buffer, Cache, Color, FamilyOwned, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use palette::{LinSrgba, Srgb, Srgba};
use tracing::warn;
use winit::dpi::PhysicalSize;

use crate::config::ScreenMessageConfig;

/// Lightweight greeting/sleep screen renderer: clears the surface to the
/// configured background colour and renders centred text using `glyphon`.
pub struct GreetingScreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    cache: Cache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    text_buffer: Buffer,
    font_system: FontSystem,
    swash_cache: SwashCache,
    font_family: FamilyOwned,
    message: String,
    background: LinSrgba<f32>,
    font_colour: LinSrgba<f32>,
    size: PhysicalSize<u32>,
    layout_dirty: bool,
    text_origin: (f32, f32),
}

impl GreetingScreen {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        screen: &ScreenMessageConfig,
    ) -> Self {
        let mut font_system = FontSystem::new();
        initialize_font_database(font_system.db_mut());
        let font_family =
            resolve_font_family(&font_system, screen.font.as_ref().map(|s| s.as_str()));
        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(32.0, 38.4));
        text_buffer.set_wrap(&mut font_system, Wrap::WordOrGlyph);

        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let swash_cache = SwashCache::new();

        let message = screen.message_or_default().into_owned();
        let background = resolve_background_colour(screen.colors.background.as_deref());
        let font_colour = resolve_font_colour(screen.colors.font.as_deref());

        let instance = GreetingScreen {
            device: device.clone(),
            queue: queue.clone(),
            format,
            cache,
            viewport,
            atlas,
            text_renderer,
            text_buffer,
            font_system,
            swash_cache,
            font_family,
            message,
            background,
            font_colour,
            size: PhysicalSize::new(0, 0),
            layout_dirty: true,
            text_origin: (0.0, 0.0),
        };
        instance
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, _scale_factor: f64) {
        if self.size != new_size {
            self.size = new_size;
            self.layout_dirty = true;
        }
    }

    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) -> bool {
        if self.size.width == 0 || self.size.height == 0 {
            return false;
        }

        let has_text = !self.message.trim().is_empty();
        let mut text_ready = true;
        if has_text {
            text_ready = self.update_layout_if_needed();
        }

        if has_text && text_ready {
            self.viewport.update(
                &self.queue,
                Resolution {
                    width: self.size.width,
                    height: self.size.height,
                },
            );

            let text_color = to_text_color(self.font_colour);
            if let Err(err) = self.text_renderer.prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [TextArea {
                    buffer: &self.text_buffer,
                    left: self.text_origin.0,
                    top: self.text_origin.1,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: self.size.width as i32,
                        bottom: self.size.height as i32,
                    },
                    default_color: text_color,
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            ) {
                warn!(error = %err, "greeting_screen_prepare_failed");
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("greeting-background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(to_wgpu_color(self.background)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            if has_text && text_ready {
                if let Err(err) = self
                    .text_renderer
                    .render(&self.atlas, &self.viewport, &mut pass)
                {
                    warn!(error = %err, "greeting_screen_draw_failed");
                }
            }
        }

        if has_text && text_ready {
            self.atlas.trim();
        }

        if has_text { text_ready } else { true }
    }

    pub fn after_submit(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Wait);
    }

    pub fn ensure_layout_ready(&mut self) -> bool {
        if self.size.width == 0 || self.size.height == 0 {
            return false;
        }
        self.update_layout_if_needed()
    }

    fn update_layout_if_needed(&mut self) -> bool {
        if !self.layout_dirty {
            return true;
        }
        if self.size.width == 0 || self.size.height == 0 {
            return false;
        }

        let font_size = compute_font_size(&self.message, self.size);
        let metrics = Metrics::new(font_size, font_size * 1.2);
        self.text_buffer.set_metrics_and_size(
            &mut self.font_system,
            metrics,
            Some(self.size.width as f32),
            Some(self.size.height as f32),
        );

        let attrs = Attrs::new().family(self.font_family.as_family());
        self.text_buffer.set_text(
            &mut self.font_system,
            &self.message,
            &attrs,
            Shaping::Advanced,
        );
        apply_center_alignment(&mut self.text_buffer);
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        self.text_origin = compute_text_origin(&self.text_buffer, self.size);
        self.layout_dirty = false;
        true
    }
}

fn compute_font_size(message: &str, size: PhysicalSize<u32>) -> f32 {
    if message.trim().is_empty() {
        return 16.0;
    }
    let min_dim = size.width.min(size.height) as f32;
    let mut scale = min_dim * 0.12; // start with 12% of the smaller side
    if scale < 24.0 {
        scale = 24.0;
    }
    if scale > 360.0 {
        scale = 360.0;
    }

    // If the message is very long, reduce the scale heuristically.
    let chars = message.chars().count().max(1);
    let adjustment = ((chars as f32) / 24.0).sqrt();
    (scale / adjustment).clamp(24.0, 360.0)
}

fn resolve_background_colour(source: Option<&str>) -> LinSrgba<f32> {
    source
        .and_then(parse_hex_color)
        .unwrap_or_else(default_background)
}

fn resolve_font_colour(source: Option<&str>) -> LinSrgba<f32> {
    source
        .and_then(parse_hex_color)
        .unwrap_or_else(default_font_colour)
}

fn initialize_font_database(db: &mut Database) {
    db.load_system_fonts();
    let bundled_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    if bundled_path.exists() {
        db.load_fonts_dir(&bundled_path);
    }
}

fn resolve_font_family(font_system: &FontSystem, requested: Option<&str>) -> FamilyOwned {
    let db = font_system.db();
    if let Some(name) = requested.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }) {
        if font_available(db, name) {
            return FamilyOwned::Name(name.into());
        }
        warn!(font = %name, "greeting_screen_font_missing");
    }

    if font_available(db, "DejaVu Sans") {
        FamilyOwned::Name("DejaVu Sans".into())
    } else {
        FamilyOwned::SansSerif
    }
}

fn font_available(db: &Database, name: &str) -> bool {
    let query = Query {
        families: &[Family::Name(name)],
        ..Default::default()
    };
    db.query(&query).is_some()
}

fn apply_center_alignment(buffer: &mut Buffer) {
    for line in &mut buffer.lines {
        line.set_align(Some(Align::Center));
    }
}

fn compute_text_origin(buffer: &Buffer, size: PhysicalSize<u32>) -> (f32, f32) {
    let mut min_top = f32::MAX;
    let mut max_bottom = f32::MIN;
    let mut has_runs = false;

    for run in buffer.layout_runs() {
        has_runs = true;
        min_top = min_top.min(run.line_top);
        max_bottom = max_bottom.max(run.line_top + run.line_height);
    }

    if !has_runs {
        return (0.0, (size.height as f32) * 0.5);
    }

    let text_height = (max_bottom - min_top).max(0.0);
    let container_height = size.height as f32;
    let top_offset = ((container_height - text_height) * 0.5) - min_top;

    (0.0, top_offset.max(0.0))
}

fn to_text_color(color: LinSrgba<f32>) -> Color {
    let srgb: Srgba<f32> = Srgba::from_linear(color);
    let srgb_u8: Srgba<u8> = srgb.into_format();
    Color::rgba(srgb_u8.red, srgb_u8.green, srgb_u8.blue, srgb_u8.alpha)
}

fn parse_hex_color(input: &str) -> Option<LinSrgba<f32>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(rgba) = Srgba::<u8>::from_str(trimmed) {
        let rgba_f32: Srgba<f32> = rgba.into_format();
        return Some(rgba_f32.into_linear());
    }

    let rgb = Srgb::<u8>::from_str(trimmed).ok()?;
    let rgba = Srgba::new(rgb.red, rgb.green, rgb.blue, 255);
    let rgba_f32: Srgba<f32> = rgba.into_format();
    Some(rgba_f32.into_linear())
}

fn to_wgpu_color(color: LinSrgba<f32>) -> wgpu::Color {
    wgpu::Color {
        r: color.red as f64,
        g: color.green as f64,
        b: color.blue as f64,
        a: color.alpha as f64,
    }
}

fn default_background() -> LinSrgba<f32> {
    parse_hex_color("#111827").unwrap()
}

fn default_font_colour() -> LinSrgba<f32> {
    parse_hex_color("#F8FAFC").unwrap()
}
