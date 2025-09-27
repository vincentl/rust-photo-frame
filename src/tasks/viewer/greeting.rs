use std::borrow::Cow;
use std::fs;
use std::path::Path;

use fontdb::Database;
use lyon_path::builder::BorderRadii;
use lyon_path::math::{point, Box2D};
use lyon_path::Path as LyonPath;
use lyon_path::Winding;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use tracing::warn;
use wgpu::util::DeviceExt;
use wgpu_glyph::ab_glyph::{FontArc, FontVec, PxScale};
use wgpu_glyph::{
    BuiltInLineBreaker, FontId, GlyphBrush, GlyphBrushBuilder, GlyphCruncher, HorizontalAlign,
    Layout, Section, SectionGeometry, Text, VerticalAlign,
};
use winit::dpi::PhysicalSize;

pub type AppConfig = crate::config::GreetingScreenConfig;

const DEFAULT_MESSAGE: &str = "Initializing…";
const DEFAULT_BACKGROUND: &str = "#101216";
const DEFAULT_FONT_COLOR: &str = "#f4f3f0";
const DEFAULT_ACCENT: &str = "#5a6c7d";

const FRAME_SHADER: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameVertex {
    position: [f32; 2],
    color: [f32; 4],
}

impl FrameVertex {
    fn layout<'a>() -> wgpu::VertexBufferLayout<'a> {
        const ATTRS: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<FrameVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}

struct Shape {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

struct TextLayoutInfo {
    screen_position: (f32, f32),
    bounds: (f32, f32),
    font_px: f32,
}

#[derive(Clone, Copy)]
struct RectPx {
    min: [f32; 2],
    max: [f32; 2],
}

impl RectPx {
    fn from_edges(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Option<Self> {
        if max_x - min_x < 1.0 || max_y - min_y < 1.0 {
            return None;
        }
        Some(Self {
            min: [min_x, min_y],
            max: [max_x, max_y],
        })
    }

    fn shrink(&self, amount: f32) -> Option<Self> {
        let amt = amount.max(0.0);
        let min_x = snap(self.min[0] + amt);
        let min_y = snap(self.min[1] + amt);
        let max_x = snap(self.max[0] - amt);
        let max_y = snap(self.max[1] - amt);
        Self::from_edges(min_x, min_y, max_x, max_y)
    }

    fn width(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    fn height(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    fn clamp_radius(&self, radius: f32) -> f32 {
        let limit = self.width().min(self.height()) * 0.5;
        radius.max(0.0).min(limit)
    }

    fn to_box2d(&self) -> Box2D {
        Box2D::new(
            point(self.min[0], self.min[1]),
            point(self.max[0], self.max[1]),
        )
    }
}

#[derive(Clone, Copy)]
struct ResolvedColors {
    background_linear: [f32; 4],
    background_color: wgpu::Color,
    font: [f32; 4],
    accent: [f32; 4],
}

impl ResolvedColors {
    fn resolve(config: &AppConfig) -> Self {
        let background =
            parse_color_choice(config.colors.background.as_deref(), DEFAULT_BACKGROUND);
        let font = parse_color_choice(config.colors.font.as_deref(), DEFAULT_FONT_COLOR);
        let accent = parse_color_choice(config.colors.accent.as_deref(), DEFAULT_ACCENT);
        let background_linear = srgb_to_linear_rgba(background);
        let font_linear = srgb_to_linear_rgba(font);
        let accent_linear = srgb_to_linear_rgba(accent);
        Self {
            background_color: wgpu::Color {
                r: background_linear[0] as f64,
                g: background_linear[1] as f64,
                b: background_linear[2] as f64,
                a: background_linear[3] as f64,
            },
            background_linear,
            font: font_linear,
            accent: accent_linear,
        }
    }
}

pub struct GreetingScreen {
    device: wgpu::Device,
    pipeline: wgpu::RenderPipeline,
    glyph_brush: GlyphBrush<()>,
    staging_belt: wgpu::util::StagingBelt,
    frame_shapes: Vec<Shape>,
    text_layout: Option<TextLayoutInfo>,
    message: String,
    font_color: [f32; 4],
    accent_color: [f32; 4],
    background_linear: [f32; 4],
    background_color: wgpu::Color,
    stroke_width_dip: f32,
    corner_radius_dip: Option<f32>,
    scale_factor: f64,
    surface_size: PhysicalSize<u32>,
    font_id: FontId,
    layout: Layout<BuiltInLineBreaker>,
    #[allow(dead_code)] // Hot-reload plumbing uses this when config watching is restored.
    current_font_key: Option<String>,
    contrast_warned: bool,
    #[allow(dead_code)] // Required when swapping fonts dynamically during config updates.
    render_format: wgpu::TextureFormat,
    #[allow(dead_code)] // Retained so config reloads can rebuild the glyph brush with a fallback.
    fallback_font: FontArc,
}

impl GreetingScreen {
    pub fn new(
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        config: &AppConfig,
    ) -> Self {
        let device = device.clone();
        let fallback_font = FontArc::try_from_slice(include_bytes!(
            "../../../assets/fonts/Inconsolata-Regular.ttf"
        ))
        .expect("fallback font must decode");

        let mut builder = GlyphBrushBuilder::using_font(fallback_font.clone());
        let font_id = FontId(0);
        let mut current_font_key = None;

        if let Some(key) = sanitize_font_key(config.font.as_deref()) {
            if let Some(custom) = load_font_by_name(&key) {
                builder = GlyphBrushBuilder::using_font(custom);
                builder.add_font(fallback_font.clone());
                current_font_key = Some(key);
            } else {
                warn!("greeting_screen.font_not_found requested={:?}", config.font);
                builder.add_font(fallback_font.clone());
            }
        } else {
            builder.add_font(fallback_font.clone());
        }

        let glyph_brush = builder.build(&device, format);
        let colors = ResolvedColors::resolve(config);
        let layout = Layout::default_wrap()
            .h_align(HorizontalAlign::Center)
            .v_align(VerticalAlign::Center);

        let mut screen = Self {
            pipeline: create_pipeline(&device, format),
            staging_belt: wgpu::util::StagingBelt::new(1024),
            frame_shapes: Vec::new(),
            text_layout: None,
            message: sanitize_message(config.message.as_deref()),
            font_color: colors.font,
            accent_color: colors.accent,
            background_linear: colors.background_linear,
            background_color: colors.background_color,
            stroke_width_dip: config.stroke_width.max(0.1),
            corner_radius_dip: config.corner_radius,
            scale_factor: 1.0,
            surface_size: PhysicalSize::new(0, 0),
            font_id,
            layout,
            current_font_key,
            contrast_warned: false,
            render_format: format,
            fallback_font,
            device,
            glyph_brush,
        };
        screen.warn_if_low_contrast();
        screen
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        if new_size.width == 0 || new_size.height == 0 {
            self.surface_size = new_size;
            self.scale_factor = scale_factor;
            self.frame_shapes.clear();
            self.text_layout = None;
            return;
        }
        self.surface_size = new_size;
        self.scale_factor = scale_factor.max(0.1);
        self.rebuild_shapes_and_layout();
    }

    #[allow(dead_code)] // Currently only used when config hot-reload is enabled.
    pub fn update_config(&mut self, config: &AppConfig) {
        let mut geometry_dirty = false;
        let mut layout_dirty = false;

        let new_message = sanitize_message(config.message.as_deref());
        if new_message != self.message {
            self.message = new_message;
            layout_dirty = true;
        }

        let new_stroke = config.stroke_width.max(0.1);
        if (self.stroke_width_dip - new_stroke).abs() > f32::EPSILON {
            self.stroke_width_dip = new_stroke;
            geometry_dirty = true;
            layout_dirty = true;
        }

        let corner_changed = match (self.corner_radius_dip, config.corner_radius) {
            (Some(a), Some(b)) => (a - b).abs() > f32::EPSILON,
            (None, None) => false,
            _ => true,
        };
        if corner_changed {
            self.corner_radius_dip = config.corner_radius;
            geometry_dirty = true;
        }

        let colors = ResolvedColors::resolve(config);
        let mut colors_changed = false;
        if self.accent_color != colors.accent {
            self.accent_color = colors.accent;
            colors_changed = true;
        }
        if self.font_color != colors.font {
            self.font_color = colors.font;
        }
        if self.background_linear != colors.background_linear {
            self.background_linear = colors.background_linear;
            self.background_color = colors.background_color;
            colors_changed = true;
        }
        if colors_changed {
            self.contrast_warned = false;
            self.warn_if_low_contrast();
            geometry_dirty = true;
        }

        let new_font_key = sanitize_font_key(config.font.as_deref());
        if new_font_key != self.current_font_key {
            self.rebuild_glyph_brush(new_font_key.as_deref());
            self.current_font_key = new_font_key;
            layout_dirty = true;
        }

        if geometry_dirty || layout_dirty {
            self.rebuild_shapes_and_layout();
        }
    }

    pub fn render(&mut self, encoder: &mut wgpu::CommandEncoder, target_view: &wgpu::TextureView) {
        if self.surface_size.width == 0 || self.surface_size.height == 0 {
            return;
        }

        if !self.frame_shapes.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("greeting-frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            for shape in &self.frame_shapes {
                pass.set_vertex_buffer(0, shape.vertex_buffer.slice(..));
                pass.set_index_buffer(shape.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..shape.index_count, 0, 0..1);
            }
        }

        if let Some(layout) = &self.text_layout {
            if !self.message.is_empty() {
                let section = Section {
                    screen_position: layout.screen_position,
                    bounds: layout.bounds,
                    text: vec![Text::new(&self.message)
                        .with_scale(layout.font_px)
                        .with_color(self.font_color)
                        .with_font_id(self.font_id)],
                    layout: self.layout,
                    ..Section::default()
                };
                self.glyph_brush.queue(section);
                if let Err(err) = self.glyph_brush.draw_queued(
                    &self.device,
                    &mut self.staging_belt,
                    encoder,
                    target_view,
                    self.surface_size.width,
                    self.surface_size.height,
                ) {
                    warn!("greeting_screen.draw_failed error={}", err);
                }
                self.staging_belt.finish();
            }
        }
    }

    pub fn on_frame_finished(&mut self) {
        self.staging_belt.recall();
    }

    pub fn background_color(&self) -> wgpu::Color {
        self.background_color
    }

    #[allow(dead_code)] // Invoked by update_config once dynamic config wiring is reintroduced.
    fn rebuild_glyph_brush(&mut self, requested: Option<&str>) {
        let mut builder = GlyphBrushBuilder::using_font(self.fallback_font.clone());
        let font_id = FontId(0);

        if let Some(key) = requested {
            if let Some(custom) = load_font_by_name(key) {
                builder = GlyphBrushBuilder::using_font(custom);
                builder.add_font(self.fallback_font.clone());
            } else {
                warn!("greeting_screen.font_not_found requested={}", key);
                builder.add_font(self.fallback_font.clone());
            }
        } else {
            builder.add_font(self.fallback_font.clone());
        }

        self.glyph_brush = builder.build(&self.device, self.render_format);
        self.font_id = font_id;
    }

    fn rebuild_shapes_and_layout(&mut self) {
        self.frame_shapes.clear();
        self.text_layout = None;

        let width = self.surface_size.width as f32;
        let height = self.surface_size.height as f32;
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let viewport = [width, height];
        let stroke_px = (self.stroke_width_dip * self.scale_factor as f32).max(1.0);
        let margin = 2.0 * stroke_px;
        let min_x = snap(margin);
        let min_y = snap(margin);
        let max_x = snap(width - margin);
        let max_y = snap(height - margin);
        let Some(outer_rect) = RectPx::from_edges(min_x, min_y, max_x, max_y) else {
            return;
        };

        let corner_base = self
            .corner_radius_dip
            .unwrap_or(self.stroke_width_dip * 0.75)
            .max(0.0)
            * self.scale_factor as f32;
        let outer_radius = outer_rect.clamp_radius(corner_base);
        let Some(outer_inner_rect) = outer_rect.shrink(stroke_px) else {
            return;
        };
        let outer_inner_radius = outer_inner_rect.clamp_radius(corner_base - stroke_px);
        let gap = 0.825 * stroke_px;
        let Some(inner_outer_rect) = outer_inner_rect.shrink(gap) else {
            return;
        };
        let inner_outer_radius = inner_outer_rect.clamp_radius(corner_base - stroke_px - gap);
        let inner_line = 0.375 * stroke_px;
        let Some(inner_inner_rect) = inner_outer_rect.shrink(inner_line) else {
            return;
        };
        let inner_inner_radius =
            inner_inner_rect.clamp_radius(corner_base - stroke_px - gap - inner_line);

        if let Some(shape) = self.build_ring_shape(
            "greeting-outer",
            viewport,
            &outer_rect,
            outer_radius,
            &outer_inner_rect,
            outer_inner_radius,
        ) {
            self.frame_shapes.push(shape);
        }
        if let Some(shape) = self.build_ring_shape(
            "greeting-inner",
            viewport,
            &inner_outer_rect,
            inner_outer_radius,
            &inner_inner_rect,
            inner_inner_radius,
        ) {
            self.frame_shapes.push(shape);
        }

        if self.message.is_empty() {
            return;
        }

        let min_dim = outer_rect.width().min(outer_rect.height());
        if min_dim <= 1.0 {
            return;
        }
        let box_size = (min_dim * (2.0 / 3.0)).max(1.0);
        let box_origin = ((width - box_size) * 0.5, (height - box_size) * 0.5);
        let layout = Layout::default_wrap()
            .h_align(HorizontalAlign::Center)
            .v_align(VerticalAlign::Center);
        self.layout = layout;
        let max_px = box_size;
        let min_px = max_px.min(12.0).max(max_px.min(6.0)).max(1.0);
        let font_px = best_fit_font_px(
            &mut self.glyph_brush,
            &self.message,
            (box_size, box_size),
            min_px,
            max_px,
        );

        let geometry = SectionGeometry {
            screen_position: box_origin,
            bounds: (box_size, box_size),
        };
        if let Some(bounds) = layout_bounds(
            &mut self.glyph_brush,
            &layout,
            &geometry,
            &self.message,
            font_px,
        ) {
            let glyph_center = ((bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5);
            let target_center = (width * 0.5, height * 0.5);
            let mut delta_x = target_center.0 - glyph_center.0;
            let mut delta_y = target_center.1 - glyph_center.1;
            let min_delta_x = box_origin.0 - bounds[0];
            let max_delta_x = box_origin.0 + box_size - bounds[2];
            let min_delta_y = box_origin.1 - bounds[1];
            let max_delta_y = box_origin.1 + box_size - bounds[3];
            delta_x = delta_x.clamp(min_delta_x, max_delta_x);
            delta_y = delta_y.clamp(min_delta_y, max_delta_y);
            self.text_layout = Some(TextLayoutInfo {
                screen_position: (box_origin.0 + delta_x, box_origin.1 + delta_y),
                bounds: (box_size, box_size),
                font_px,
            });
        }
    }

    fn build_ring_shape(
        &self,
        label: &str,
        viewport: [f32; 2],
        outer: &RectPx,
        outer_radius: f32,
        inner: &RectPx,
        inner_radius: f32,
    ) -> Option<Shape> {
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return None;
        }
        let mut builder = LyonPath::builder();
        builder.add_rounded_rectangle(
            &outer.to_box2d(),
            &BorderRadii::new(outer_radius.max(0.0)),
            Winding::Positive,
        );
        builder.add_rounded_rectangle(
            &inner.to_box2d(),
            &BorderRadii::new(inner_radius.max(0.0)),
            Winding::Negative,
        );
        let path = builder.build();
        let mut buffers: VertexBuffers<FrameVertex, u16> = VertexBuffers::new();
        let mut tessellator = FillTessellator::new();
        let result = tessellator.tessellate_path(
            &path,
            &FillOptions::default(),
            &mut BuffersBuilder::new(&mut buffers, |vertex: FillVertex| FrameVertex {
                position: to_clip(vertex.position().to_array(), viewport),
                color: self.accent_color,
            }),
        );
        if let Err(err) = result {
            warn!(
                "greeting_screen.tessellation_failed label={} error={}",
                label, err
            );
            return None;
        }
        if buffers.vertices.is_empty() || buffers.indices.is_empty() {
            return None;
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&buffers.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("{}-indices", label)),
                contents: bytemuck::cast_slice(&buffers.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        Some(Shape {
            vertex_buffer,
            index_buffer,
            index_count: buffers.indices.len() as u32,
        })
    }

    fn warn_if_low_contrast(&mut self) {
        if self.contrast_warned {
            return;
        }
        let bg_lum = relative_luminance(self.background_linear);
        let fg_lum = relative_luminance(self.font_color);
        let (bright, dark) = if bg_lum > fg_lum {
            (bg_lum, fg_lum)
        } else {
            (fg_lum, bg_lum)
        };
        let ratio = (bright + 0.05) / (dark + 0.05);
        if ratio < 4.5 {
            warn!("greeting_screen.low_contrast ratio={:.2}", ratio);
            self.contrast_warned = true;
        }
    }
}

pub fn best_fit_font_px(
    brush: &mut GlyphBrush<()>,
    text: &str,
    box_px: (f32, f32),
    min_px: f32,
    max_px: f32,
) -> f32 {
    if text.trim().is_empty() {
        return min_px.max(1.0);
    }
    if brush.fonts().is_empty() {
        return min_px.max(1.0);
    }
    if box_px.0 <= 0.0 || box_px.1 <= 0.0 {
        return min_px.max(1.0);
    }
    if !min_px.is_finite() || !max_px.is_finite() {
        return 12.0;
    }
    let mut low = min_px.min(max_px).max(1.0);
    let mut high = max_px.max(low);
    if high <= 1.0 {
        return 1.0;
    }
    let layout = Layout::default_wrap()
        .h_align(HorizontalAlign::Center)
        .v_align(VerticalAlign::Center);
    let geometry = SectionGeometry {
        screen_position: (0.0, 0.0),
        bounds: box_px,
    };
    if fits_bounds(brush, &layout, &geometry, text, high) {
        return high.clamp(min_px.min(max_px).max(1.0), max_px.max(min_px).max(1.0));
    }
    if !fits_bounds(brush, &layout, &geometry, text, low) {
        return low.clamp(min_px.min(max_px).max(1.0), max_px.max(min_px).max(1.0));
    }
    let mut best = low;
    for _ in 0..18 {
        if high - low <= 0.5 {
            break;
        }
        let mid = (low + high) * 0.5;
        if fits_bounds(brush, &layout, &geometry, text, mid) {
            best = mid;
            low = mid;
        } else {
            high = mid;
        }
    }
    best.clamp(min_px.min(max_px).max(1.0), max_px.max(min_px).max(1.0))
}

fn fits_bounds(
    brush: &mut GlyphBrush<()>,
    layout: &Layout<BuiltInLineBreaker>,
    geometry: &SectionGeometry,
    text: &str,
    font_px: f32,
) -> bool {
    if let Some(bounds) = layout_bounds(brush, layout, geometry, text, font_px) {
        let width = bounds[2] - bounds[0];
        let height = bounds[3] - bounds[1];
        width <= geometry.bounds.0 + 0.5 && height <= geometry.bounds.1 + 0.5
    } else {
        true
    }
}

fn layout_bounds(
    brush: &mut GlyphBrush<()>,
    layout: &Layout<BuiltInLineBreaker>,
    geometry: &SectionGeometry,
    text: &str,
    font_px: f32,
) -> Option<[f32; 4]> {
    let section = Section {
        screen_position: geometry.screen_position,
        bounds: geometry.bounds,
        text: vec![Text::new(text)
            .with_scale(PxScale::from(font_px))
            .with_font_id(FontId::default())],
        layout: *layout,
        ..Section::default()
    };
    brush
        .glyph_bounds_custom_layout(section, layout)
        .map(|rect| [rect.min.x, rect.min.y, rect.max.x, rect.max.y])
}

fn sanitize_message(raw: Option<&str>) -> String {
    let Some(text) = raw else {
        return DEFAULT_MESSAGE.to_string();
    };
    if text.trim().is_empty() {
        DEFAULT_MESSAGE.to_string()
    } else {
        text.to_string()
    }
}

fn sanitize_font_key(raw: Option<&str>) -> Option<String> {
    let text = raw?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn load_font_by_name(name: &str) -> Option<FontArc> {
    load_font_from_assets(name).or_else(|| load_system_font(name))
}

fn load_font_from_assets(requested: &str) -> Option<FontArc> {
    let fonts_dir = Path::new("assets/fonts");
    let entries = fs::read_dir(fonts_dir).ok()?;
    let requested_lower = requested.to_lowercase();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext_ok = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| matches!(ext.to_lowercase().as_str(), "ttf" | "otf"))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        let stem_lower = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let file_lower = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let matches_request = stem_lower
            .as_deref()
            .map(|s| s == requested_lower)
            .unwrap_or(false)
            || file_lower
                .as_deref()
                .map(|s| s == requested_lower)
                .unwrap_or(false);
        if matches_request {
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(font) = FontArc::try_from_vec(bytes) {
                    return Some(font);
                }
            }
        }
    }
    None
}

fn load_system_font(name: &str) -> Option<FontArc> {
    let mut db = Database::new();
    db.load_system_fonts();
    let requested_lower = name.to_lowercase();
    let face_id = db.faces().find_map(|face| {
        let mut matches_family = face
            .families
            .iter()
            .any(|(family, _)| family.to_lowercase() == requested_lower);
        if !matches_family {
            matches_family = face.post_script_name.to_lowercase() == requested_lower;
        }
        matches_family.then_some(face.id)
    })?;
    db.with_face_data(face_id, |data, index| {
        FontVec::try_from_vec_and_index(data.to_vec(), index)
            .ok()
            .map(FontArc::new)
    })?
}

fn create_pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("greeting-frame-shader"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(FRAME_SHADER)),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("greeting-frame-layout"),
        bind_group_layouts: &[],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("greeting-frame"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[FrameVertex::layout()],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

fn parse_color_choice(value: Option<&str>, fallback: &str) -> [f32; 4] {
    value
        .and_then(|s| parse_hex_color(s.trim()))
        .or_else(|| {
            warn!(
                "greeting_screen.invalid_color value={}",
                value.unwrap_or("")
            );
            None
        })
        .unwrap_or_else(|| parse_hex_color(fallback).expect("fallback color valid"))
}

fn parse_hex_color(value: &str) -> Option<[f32; 4]> {
    let hex = value.trim().trim_start_matches('#');
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
        }
        4 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            let a = u8::from_str_radix(&hex[3..4].repeat(2), 16).ok()?;
            Some([
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                a as f32 / 255.0,
            ])
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some([
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                a as f32 / 255.0,
            ])
        }
        _ => None,
    }
}

fn srgb_to_linear_rgba(color: [f32; 4]) -> [f32; 4] {
    [
        srgb_to_linear(color[0]),
        srgb_to_linear(color[1]),
        srgb_to_linear(color[2]),
        color[3],
    ]
}

fn srgb_to_linear(component: f32) -> f32 {
    if component <= 0.04045 {
        component / 12.92
    } else {
        ((component + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance(color: [f32; 4]) -> f32 {
    0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2]
}

fn to_clip(position: [f32; 2], viewport: [f32; 2]) -> [f32; 2] {
    let x = (position[0] / viewport[0]) * 2.0 - 1.0;
    let y = 1.0 - (position[1] / viewport[1]) * 2.0;
    [x, y]
}

fn snap(value: f32) -> f32 {
    value.round()
}
