use std::path::PathBuf;

use ab_glyph::FontArc;
use bytemuck::{Pod, Zeroable};
use fontdb::{Database, Family, Query};
use lyon::math::{Box2D, point};
use lyon::path::builder::BorderRadii;
use lyon::path::{Path, Winding};
use lyon::tessellation::{
    BuffersBuilder, StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers,
};
use tracing::warn;
use wgpu::util::{DeviceExt, StagingBelt};
use wgpu_glyph::GlyphCruncher;
use wgpu_glyph::{
    BuiltInLineBreaker, GlyphBrush, GlyphBrushBuilder, HorizontalAlign, Layout, Section, Text,
    VerticalAlign,
};
use winit::dpi::PhysicalSize;

use crate::config::{GreetingScreenColorsConfig, ScreenMessageConfig};

const MEASURE_BOUNDS_EXTENT: f32 = 100_000.0;
const DRAW_BOUNDS_PADDING: f32 = 2.0;

const FRAME_SHADER: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) in_pos: vec2<f32>, @location(1) in_color: vec4<f32>) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(in_pos, 0.0, 1.0);
    out.color = in_color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameVertex {
    position: [f32; 2],
    color: [f32; 4],
}

struct FrameMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

#[derive(Clone, Debug)]
struct GreetingColors {
    background: wgpu::Color,
    font: [f32; 4],
    accent: [f32; 4],
}

#[derive(Clone, Debug)]
struct GreetingSettings {
    message: String,
    font_name: Option<String>,
    stroke_width_dip: f32,
    corner_radius_dip: f32,
    colors: GreetingColors,
}

#[derive(Clone, Copy, Debug, Default)]
struct TextLayout {
    font_size: f32,
    bounds: (f32, f32),
    screen_position: (f32, f32),
}

/// GPU-backed renderer for the configurable greeting screen.
///
/// The component owns the glyph brush, frame pipeline, and cached layout so
/// callers only need to notify it about resizes or configuration changes before
/// issuing a `render` call.
pub struct GreetingScreen {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    glyph_brush: GlyphBrush<()>,
    staging_belt: StagingBelt,
    frame_pipeline: wgpu::RenderPipeline,
    frame_mesh: Option<FrameMesh>,
    layout: Option<TextLayout>,
    size: PhysicalSize<u32>,
    scale_factor: f64,
    settings: GreetingSettings,
    loaded_font_request: Option<String>,
    glyph_cache_side: u32,
    glyph_texture_limit: f32,
}

impl GreetingScreen {
    pub fn new(
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        screen: &ScreenMessageConfig,
    ) -> Self {
        let settings = GreetingSettings::from_screen(screen);
        let glyph_font = load_font(&settings.font_name);
        let device_limits = device.limits();
        let glyph_texture_limit = device_limits.max_texture_dimension_2d.min(4096);
        let glyph_cache_side = glyph_texture_limit
            .min(2048)
            .max(256)
            .min(glyph_texture_limit);
        let glyph_brush = build_glyph_brush(glyph_font, device, format, glyph_cache_side);
        let frame_pipeline = build_frame_pipeline(device, format);
        if contrast_ratio(settings.colors.font, settings.colors.background) < 4.5 {
            warn!("greeting_screen_low_contrast");
        }
        let font_request = settings.font_name.clone();
        Self {
            device: device.clone(),
            format,
            glyph_brush,
            staging_belt: StagingBelt::new(1024),
            frame_pipeline,
            frame_mesh: None,
            layout: None,
            size: PhysicalSize::new(0, 0),
            scale_factor: 1.0,
            settings,
            loaded_font_request: font_request,
            glyph_cache_side,
            glyph_texture_limit: glyph_texture_limit as f32,
        }
    }

    pub fn update_screen(&mut self, screen: &ScreenMessageConfig) {
        self.settings = GreetingSettings::from_screen(screen);
        self.loaded_font_request = None;
        self.rebuild_geometry();
        self.update_text_layout();
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        if self.size != new_size || (self.scale_factor - scale_factor).abs() > f64::EPSILON {
            self.size = new_size;
            self.scale_factor = scale_factor;
            self.rebuild_geometry();
            self.update_text_layout();
        }
    }

    pub fn render(&mut self, encoder: &mut wgpu::CommandEncoder, target_view: &wgpu::TextureView) {
        if self.size.width == 0 || self.size.height == 0 {
            return;
        }
        let layout = match self.layout {
            Some(layout) => layout,
            None => {
                self.update_text_layout();
                match self.layout {
                    Some(layout) => layout,
                    None => return,
                }
            }
        };
        self.staging_belt.recall();

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("greeting-frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.settings.colors.background),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        if let Some(mesh) = self.frame_mesh.as_ref() {
            pass.set_pipeline(&self.frame_pipeline);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
        drop(pass);

        let section = build_section(&self.settings, layout);
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
    }

    pub fn screen_message(
        &mut self,
        screen: &ScreenMessageConfig,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) {
        self.update_screen(screen);
        self.render(encoder, target_view);
    }

    fn ensure_font_loaded(&mut self) {
        if self.loaded_font_request == self.settings.font_name {
            return;
        }
        let font = load_font(&self.settings.font_name);
        self.glyph_brush =
            build_glyph_brush(font, &self.device, self.format, self.glyph_cache_side);
        self.loaded_font_request = self.settings.font_name.clone();
    }

    fn rebuild_geometry(&mut self) {
        self.frame_mesh =
            build_frame_mesh(&self.device, &self.settings, self.size, self.scale_factor);
    }

    fn update_text_layout(&mut self) {
        if self.size.width == 0 || self.size.height == 0 {
            self.layout = None;
            return;
        }
        self.ensure_font_loaded();
        let bounds_side = (self.size.width.min(self.size.height) as f32) * (2.0 / 3.0);
        if bounds_side <= 0.0 {
            self.layout = None;
            return;
        }
        let box_size = (bounds_side, bounds_side);
        let min_px = 8.0;
        let max_px = bounds_side.max(8.0);
        let font_size = best_fit_font_px(
            &mut self.glyph_brush,
            &self.settings.message,
            box_size,
            min_px,
            max_px,
            self.glyph_texture_limit,
        );
        let measure_layout = measurement_layout();
        let (measured_width, measured_height) = measure_text_extent(
            &mut self.glyph_brush,
            &self.settings.message,
            font_size,
            measure_layout,
        )
        .unwrap_or((0.0, 0.0));
        let clamped_width = measured_width.min(box_size.0);
        let clamped_height = measured_height.min(box_size.1);
        let padded_bounds = (
            (clamped_width + DRAW_BOUNDS_PADDING).max(1.0),
            (clamped_height + DRAW_BOUNDS_PADDING).max(1.0),
        );
        let screen_center = (
            (self.size.width as f32) * 0.5,
            (self.size.height as f32) * 0.5,
        );
        self.layout = Some(TextLayout {
            font_size,
            bounds: padded_bounds,
            screen_position: screen_center,
        });
    }
}

fn build_section(settings: &GreetingSettings, layout: TextLayout) -> Section<'_> {
    Section {
        screen_position: layout.screen_position,
        bounds: (layout.bounds.0, layout.bounds.1),
        layout: Layout::default_wrap()
            .h_align(HorizontalAlign::Center)
            .v_align(VerticalAlign::Center),
        text: vec![
            Text::new(settings.message.as_str())
                .with_scale(layout.font_size)
                .with_color(settings.colors.font),
        ],
        ..Section::default()
    }
}

pub fn best_fit_font_px(
    brush: &mut GlyphBrush<()>,
    text: &str,
    box_px: (f32, f32),
    min_px: f32,
    max_px: f32,
    texture_limit_px: f32,
) -> f32 {
    if text.trim().is_empty() {
        return min_px.max(1.0);
    }
    let glyph_count = text.chars().filter(|c| !c.is_control()).count().max(1) as f32;
    let texture_limit_px = texture_limit_px.max(1.0);
    let texture_cap = (texture_limit_px * texture_limit_px / glyph_count).sqrt() * 0.9;
    let mut low = min_px.max(1.0);
    let mut high = max_px.min(texture_cap.max(low)).max(low);
    let mut best = low;
    let layout = measurement_layout();
    for _ in 0..18 {
        let mid = (low + high) * 0.5;
        let fits = measure_text_extent(brush, text, mid, layout)
            .map(|(width, height)| width <= box_px.0 + 0.5 && height <= box_px.1 + 0.5)
            .unwrap_or(false);
        if fits {
            best = mid;
            low = mid;
        } else {
            high = mid;
        }
        if (high - low).abs() < 0.5 {
            break;
        }
    }
    best
}

fn measurement_layout() -> Layout<BuiltInLineBreaker> {
    Layout::default_wrap()
        .h_align(HorizontalAlign::Left)
        .v_align(VerticalAlign::Top)
}

fn measure_text_extent(
    brush: &mut GlyphBrush<()>,
    text: &str,
    font_size: f32,
    layout: Layout<BuiltInLineBreaker>,
) -> Option<(f32, f32)> {
    if text.trim().is_empty() {
        return Some((0.0, 0.0));
    }
    let bounds = (MEASURE_BOUNDS_EXTENT, MEASURE_BOUNDS_EXTENT);
    let section_layout = layout;
    let section = Section {
        screen_position: (0.0, 0.0),
        bounds,
        layout: section_layout,
        text: vec![Text::new(text).with_scale(font_size)],
        ..Section::default()
    };
    brush
        .glyph_bounds_custom_layout(section, &section_layout)
        .map(|rect| (rect.max.x - rect.min.x, rect.max.y - rect.min.y))
}

fn build_glyph_brush(
    font: FontArc,
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    cache_side: u32,
) -> GlyphBrush<()> {
    let side = cache_side.max(1);
    GlyphBrushBuilder::using_font(font)
        .initial_cache_size((side, side))
        .build(device, format)
}

fn build_frame_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("greeting-frame-shader"),
        source: wgpu::ShaderSource::Wgsl(FRAME_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("greeting-frame-layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("greeting-frame-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<FrameVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview: None,
        cache: None,
    })
}

fn build_frame_mesh(
    device: &wgpu::Device,
    settings: &GreetingSettings,
    size: PhysicalSize<u32>,
    scale_factor: f64,
) -> Option<FrameMesh> {
    if size.width == 0 || size.height == 0 {
        return None;
    }
    let width_px = size.width as f32;
    let height_px = size.height as f32;
    let scale = scale_factor as f32;
    let stroke = (settings.stroke_width_dip * scale).max(0.5);
    let outer_margin = stroke * 2.0;
    let outer_line_width = stroke;
    let gap = stroke * 0.825;
    let inner_line_width = stroke * 0.375;
    let corner_radius = (settings.corner_radius_dip * scale).max(0.0);

    let outer_center_margin = outer_margin + outer_line_width * 0.5;
    let inner_center_margin = outer_margin + outer_line_width + gap + inner_line_width * 0.5;
    if outer_center_margin * 2.0 >= width_px
        || outer_center_margin * 2.0 >= height_px
        || inner_center_margin * 2.0 >= width_px
        || inner_center_margin * 2.0 >= height_px
    {
        return None;
    }

    let outer_path = rounded_rect(outer_center_margin, corner_radius, width_px, height_px)?;
    let inner_radius_offset = outer_line_width * 0.5 + gap + inner_line_width * 0.5;
    let inner_radius = (corner_radius - inner_radius_offset).max(inner_line_width * 0.5);
    let inner_path = rounded_rect(inner_center_margin, inner_radius, width_px, height_px)?;

    let mut geometry: VertexBuffers<[f32; 2], u16> = VertexBuffers::new();
    let mut tess = StrokeTessellator::new();
    let mut options = StrokeOptions::default();
    options.line_width = outer_line_width;
    options.tolerance = 0.1;
    options.start_cap = lyon::tessellation::LineCap::Round;
    options.end_cap = lyon::tessellation::LineCap::Round;
    options.line_join = lyon::tessellation::LineJoin::Round;
    if tess
        .tessellate_path(
            outer_path.as_slice(),
            &options,
            &mut BuffersBuilder::new(&mut geometry, |vertex: StrokeVertex| {
                vertex.position().to_array()
            }),
        )
        .is_err()
    {
        return None;
    }
    options.line_width = inner_line_width;
    if tess
        .tessellate_path(
            inner_path.as_slice(),
            &options,
            &mut BuffersBuilder::new(&mut geometry, |vertex: StrokeVertex| {
                vertex.position().to_array()
            }),
        )
        .is_err()
    {
        return None;
    }

    let mut vertices = Vec::with_capacity(geometry.vertices.len());
    for position in geometry.vertices.iter().copied() {
        let nx = (position[0] / width_px) * 2.0 - 1.0;
        let ny = 1.0 - (position[1] / height_px) * 2.0;
        vertices.push(FrameVertex {
            position: [nx, ny],
            color: settings.colors.accent,
        });
    }
    let indices = geometry.indices;
    if indices.is_empty() || vertices.is_empty() {
        return None;
    }
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("greeting-frame-vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("greeting-frame-indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    Some(FrameMesh {
        vertex_buffer,
        index_buffer,
        index_count: indices.len() as u32,
    })
}

fn rounded_rect(
    center_margin: f32,
    radius: f32,
    width: f32,
    height: f32,
) -> Option<lyon::path::Path> {
    let min_x = snap_to_half(center_margin);
    let min_y = snap_to_half(center_margin);
    let max_x = snap_to_half(width - center_margin);
    let max_y = snap_to_half(height - center_margin);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    let rect = Box2D::new(point(min_x, min_y), point(max_x, max_y));
    let width = (max_x - min_x).max(0.0);
    let height = (max_y - min_y).max(0.0);
    let max_radius = 0.5 * width.min(height);
    let radius = radius.clamp(0.0, max_radius);
    let mut builder = Path::builder();
    let radii = BorderRadii::new(radius);
    builder.add_rounded_rectangle(&rect, &radii, Winding::Positive);
    Some(builder.build())
}

fn snap_to_half(value: f32) -> f32 {
    (value * 2.0).round() * 0.5
}

fn load_font(request: &Option<String>) -> FontArc {
    if let Some(name) = request.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some(font) = load_named_font(name) {
            return font;
        }
        warn!(font = %name, "greeting_screen_font_missing");
    }

    // reasonable default: DejaVu Sans
    load_named_font("DejaVu Sans")
        .unwrap_or_else(|| panic!("Default system font (DejaVu Sans) not found"))
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
                FontArc::try_from_vec(data.as_ref().as_ref().to_vec()).ok()
            }
            fontdb::Source::File(path) => std::fs::read(path)
                .ok()
                .and_then(|bytes| FontArc::try_from_vec(bytes).ok()),
            fontdb::Source::SharedFile(_, data) => {
                FontArc::try_from_vec(data.as_ref().as_ref().to_vec()).ok()
            }
        }
    })
}

fn contrast_ratio(foreground: [f32; 4], background: wgpu::Color) -> f32 {
    let bg = [
        background.r as f32,
        background.g as f32,
        background.b as f32,
        background.a as f32,
    ];
    let l1 = relative_luminance(foreground);
    let l2 = relative_luminance(bg);
    let (light, dark) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (light + 0.05) / (dark + 0.05)
}

fn relative_luminance(color: [f32; 4]) -> f32 {
    0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2]
}

impl GreetingSettings {
    fn from_screen(screen: &ScreenMessageConfig) -> Self {
        let message = screen.message_or_default().into_owned();
        let font_name = screen.font.clone();
        let stroke_width_dip = screen.effective_stroke_width_dip();
        let corner_radius_dip = screen.effective_corner_radius_dip(stroke_width_dip);
        let colors = GreetingColors::from_config(&screen.colors);
        Self {
            message,
            font_name,
            stroke_width_dip,
            corner_radius_dip,
            colors,
        }
    }
}

impl GreetingColors {
    fn from_config(colors: &GreetingScreenColorsConfig) -> Self {
        let background = resolve_color(&colors.background, default_background_srgb());
        let font = resolve_color(&colors.font, default_font_srgb());
        let accent = resolve_color(&colors.accent, default_accent_srgb());
        Self {
            background: to_wgpu_color(background),
            font,
            accent,
        }
    }
}

fn resolve_color(source: &Option<String>, default: [f32; 4]) -> [f32; 4] {
    if let Some(raw) = source.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        match parse_hex_color(raw) {
            Ok(color) => return color,
            Err(err) => warn!(value = raw, error = %err, "greeting_screen_color_parse_failed"),
        }
    }
    default
}

fn parse_hex_color(input: &str) -> Result<[f32; 4], String> {
    let trimmed = input.trim().trim_start_matches('#');
    let (r, g, b, a) = match trimmed.len() {
        3 => (
            expand_hex_digit(trimmed.as_bytes()[0])?,
            expand_hex_digit(trimmed.as_bytes()[1])?,
            expand_hex_digit(trimmed.as_bytes()[2])?,
            255,
        ),
        4 => (
            expand_hex_digit(trimmed.as_bytes()[0])?,
            expand_hex_digit(trimmed.as_bytes()[1])?,
            expand_hex_digit(trimmed.as_bytes()[2])?,
            expand_hex_digit(trimmed.as_bytes()[3])?,
        ),
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
        _ => return Err("unsupported color length".into()),
    };
    Ok([
        srgb_to_linear(r),
        srgb_to_linear(g),
        srgb_to_linear(b),
        (a as f32) / 255.0,
    ])
}

fn expand_hex_digit(byte: u8) -> Result<u8, String> {
    let value = hex_value(byte)?;
    Ok((value << 4) | value)
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

fn default_background_srgb() -> [f32; 4] {
    srgb_tuple_to_linear(0x11, 0x18, 0x27, 0xFF)
}

fn default_font_srgb() -> [f32; 4] {
    srgb_tuple_to_linear(0xF8, 0xFA, 0xFC, 0xFF)
}

fn default_accent_srgb() -> [f32; 4] {
    srgb_tuple_to_linear(0x38, 0xBD, 0xF8, 0xFF)
}

fn srgb_tuple_to_linear(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [
        srgb_to_linear(r),
        srgb_to_linear(g),
        srgb_to_linear(b),
        (a as f32) / 255.0,
    ]
}
