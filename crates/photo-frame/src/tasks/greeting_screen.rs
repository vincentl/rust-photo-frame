use std::path::PathBuf;

use ab_glyph::FontArc;
use fontdb::{Database, Family, Query};
use tracing::warn;
use wgpu::util::StagingBelt;
use wgpu_glyph::{
    GlyphBrush, GlyphBrushBuilder, HorizontalAlign, Layout, Section, Text, VerticalAlign,
};
use winit::dpi::PhysicalSize;

use crate::config::ScreenMessageConfig;

/// Lightweight greeting/sleep screen renderer: clears the surface to the
/// configured background colour and renders centred text using `wgpu_glyph`.
pub struct GreetingScreen {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    glyph_brush: GlyphBrush<()>,
    staging_belt: StagingBelt,
    font: FontArc,
    message: String,
    background: wgpu::Color,
    font_colour: [f32; 4],
    size: PhysicalSize<u32>,
}

impl GreetingScreen {
    pub fn new(
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        screen: &ScreenMessageConfig,
    ) -> Self {
        let font = load_font(&screen.font);
        let glyph_brush = GlyphBrushBuilder::using_font(font.clone()).build(device, format);
        let staging_belt = StagingBelt::new(1024);
        let mut instance = GreetingScreen {
            device: device.clone(),
            format,
            glyph_brush,
            staging_belt,
            font,
            message: String::new(),
            background: default_background(),
            font_colour: default_font_colour(),
            size: PhysicalSize::new(0, 0),
        };
        instance.update_screen(screen);
        instance
    }

    pub fn update_screen(&mut self, screen: &ScreenMessageConfig) {
        self.message = screen.message_or_default().into_owned();
        self.background = resolve_background_colour(&screen.colors.background);
        self.font_colour = resolve_font_colour(&screen.colors.font);
        if let Some(font_name) = screen.font.as_ref() {
            if let Some(new_font) = load_named_font(font_name.trim()) {
                self.glyph_brush = GlyphBrushBuilder::using_font(new_font.clone())
                    .build(&self.device, self.format);
                self.font = new_font;
            } else {
                warn!(font = %font_name, "greeting_screen_font_missing");
            }
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, _scale_factor: f64) {
        self.size = new_size;
    }

    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) -> bool {
        if self.size.width == 0 || self.size.height == 0 {
            return false;
        }

        // Clear to background colour.
        {
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("greeting-background"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
        }

        // Queue a simple debug glyph so we can inspect rendering behaviour without
        // pulling configuration data. Drawing a bold "X" makes it obvious whether
        // the greeting scene reaches the screen and the glyph brush is working.
        let section = Section {
            screen_position: (
                (self.size.width as f32) * 0.5,
                (self.size.height as f32) * 0.5,
            ),
            bounds: (self.size.width as f32, self.size.height as f32),
            layout: Layout::default_wrap()
                .h_align(HorizontalAlign::Center)
                .v_align(VerticalAlign::Center),
            text: vec![
                Text::new("X")
                    .with_scale(120.0)
                    .with_color([1.0, 0.0, 0.0, 1.0]),
            ],
            ..Section::default()
        };
        self.glyph_brush.queue(section);
        if let Err(err) = self.glyph_brush.draw_queued(
            &self.device,
            &mut self.staging_belt,
            encoder,
            target_view,
            self.size.width,
            self.size.height,
        ) {
            warn!(error = %err, "greeting_screen_draw_failed");
        }

        self.staging_belt.finish();
        true
    }

    pub fn after_submit(&mut self) {
        self.staging_belt.recall();
        let _ = self.device.poll(wgpu::PollType::Wait);
    }

    pub fn ensure_layout_ready(&mut self) -> bool {
        self.size.width > 0 && self.size.height > 0
    }

    pub fn screen_message(
        &mut self,
        screen: &ScreenMessageConfig,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) -> bool {
        self.update_screen(screen);
        self.render(encoder, target_view)
    }
}

fn resolve_background_colour(source: &Option<String>) -> wgpu::Color {
    source
        .as_ref()
        .and_then(|value| parse_hex_color(value).ok())
        .map(to_wgpu_color)
        .unwrap_or_else(default_background)
}

fn resolve_font_colour(source: &Option<String>) -> [f32; 4] {
    source
        .as_ref()
        .and_then(|value| parse_hex_color(value).ok())
        .unwrap_or_else(default_font_colour)
}

fn parse_hex_color(input: &str) -> Result<[f32; 4], String> {
    let trimmed = input.trim().trim_start_matches('#');
    let (r, g, b, a) = match trimmed.len() {
        6 => (
            parse_hex_pair(&trimmed[0..2])?,
            parse_hex_pair(&trimmed[2..4])?,
            parse_hex_pair(&trimmed[4..6])?,
            255,
        ),
        8 => (
            parse_hex_pair(&trimmed[0..2])?,
            parse_hex_pair(&trimmed[2..4])?,
            parse_hex_pair(&trimmed[4..6])?,
            parse_hex_pair(&trimmed[6..8])?,
        ),
        _ => return Err("unsupported colour length".into()),
    };
    Ok([
        srgb_to_linear(r),
        srgb_to_linear(g),
        srgb_to_linear(b),
        (a as f32) / 255.0,
    ])
}

fn parse_hex_pair(slice: &str) -> Result<u8, String> {
    let bytes = slice.as_bytes();
    if bytes.len() != 2 {
        return Err("invalid pair".into());
    }
    let hi = hex_value(bytes[0])?;
    let lo = hex_value(bytes[1])?;
    Ok((hi << 4) | lo)
}

fn hex_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("invalid hex digit".into()),
    }
}

fn srgb_to_linear(component: u8) -> f32 {
    let srgb = component as f32 / 255.0;
    if srgb <= 0.04045 {
        srgb / 12.92
    } else {
        ((srgb + 0.055) / 1.055).powf(2.4)
    }
}

fn to_wgpu_color(color: [f32; 4]) -> wgpu::Color {
    wgpu::Color {
        r: color[0] as f64,
        g: color[1] as f64,
        b: color[2] as f64,
        a: color[3] as f64,
    }
}

fn default_background() -> wgpu::Color {
    to_wgpu_color(parse_hex_color("#111827").unwrap())
}

fn default_font_colour() -> [f32; 4] {
    parse_hex_color("#F8FAFC").unwrap()
}

fn load_font(request: &Option<String>) -> FontArc {
    if let Some(name) = request.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some(font) = load_named_font(name) {
            return font;
        }
        warn!(font = %name, "greeting_screen_font_missing");
    }
    load_named_font("DejaVu Sans").unwrap_or_else(|| panic!("Default system font not found"))
}

fn load_named_font(name: &str) -> Option<FontArc> {
    let mut db = Database::new();
    db.load_system_fonts();
    let bundled_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    if bundled_path.exists() {
        db.load_fonts_dir(&bundled_path);
    }
    let query = Query {
        families: &[Family::Name(name)],
        ..Default::default()
    };
    db.query(&query).and_then(|id| {
        let face = db.face(id)?;
        match &face.source {
            fontdb::Source::Binary(data) => {
                let bytes = data.as_ref().as_ref();
                FontArc::try_from_vec(bytes.to_vec()).ok()
            }
            fontdb::Source::File(path) => std::fs::read(path)
                .ok()
                .and_then(|bytes| FontArc::try_from_vec(bytes).ok()),
            fontdb::Source::SharedFile(_, data) => {
                let bytes = data.as_ref().as_ref();
                FontArc::try_from_vec(bytes.to_vec()).ok()
            }
        }
    })
}
