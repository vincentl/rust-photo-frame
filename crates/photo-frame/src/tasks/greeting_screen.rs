use std::num::NonZeroU64;
use std::path::PathBuf;
use std::str::FromStr;

use bytemuck::{Pod, Zeroable};
use fontdb::{Database, Family, Query};
use glyphon::cosmic_text::Align;
use glyphon::{
    Attrs, Buffer, Cache, Color, FamilyOwned, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use lyon::math::{Box2D, point};
use lyon::path::{Path, Winding, builder::BorderRadii};
use lyon::tessellation::{
    BuffersBuilder, LineCap, LineJoin, StrokeOptions, StrokeTessellator, StrokeVertex,
    StrokeVertexConstructor, TessellationError, VertexBuffers,
};
use palette::{LinSrgba, Srgb, Srgba};
use tracing::warn;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

use crate::config::ScreenMessageConfig;

/// Lightweight greeting/sleep screen renderer: clears the surface to the
/// configured background colour and renders centred text using `glyphon`.
pub struct GreetingScreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    _format: wgpu::TextureFormat,
    _cache: Cache,
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
    accent_colour: LinSrgba<f32>,
    stroke_width_dip: f32,
    corner_radius_dip: f32,
    size: PhysicalSize<u32>,
    scale_factor: f64,
    text_origin: (f32, f32),
    frame_pipeline: wgpu::RenderPipeline,
    frame_bind_group: wgpu::BindGroup,
    frame_uniform_buffer: wgpu::Buffer,
    frame_vertex_buffer: wgpu::Buffer,
    frame_index_buffer: wgpu::Buffer,
    frame_index_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameVertex {
    position: [f32; 2],
}

struct FrameVertexCtor;

impl StrokeVertexConstructor<FrameVertex> for FrameVertexCtor {
    fn new_vertex(&mut self, vertex: StrokeVertex) -> FrameVertex {
        let pos = vertex.position();
        FrameVertex {
            position: [pos.x, pos.y],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniforms {
    color: [f32; 4],
    screen_size: [f32; 2],
    _pad: [f32; 2],
}

const MIN_FRAME_PADDING_DIP: f32 = 48.0;
const FRAME_GAP_MULTIPLIER: f32 = 0.5;

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
        let accent_colour = resolve_accent_colour(screen.colors.accent.as_deref());
        let stroke_width_dip = screen.effective_stroke_width_dip();
        let corner_radius_dip = screen.effective_corner_radius_dip(stroke_width_dip);

        let frame_uniforms = FrameUniforms {
            color: to_linear_array(accent_colour),
            screen_size: [1.0, 1.0],
            _pad: [0.0; 2],
        };
        let frame_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("greeting-frame-uniforms"),
            contents: bytemuck::bytes_of(&frame_uniforms),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let frame_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("greeting-frame-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/greeting_frame.wgsl").into()),
        });
        let frame_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("greeting-frame-bind-group-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(
                            std::mem::size_of::<FrameUniforms>() as u64
                        ),
                    },
                    count: None,
                }],
            });
        let frame_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("greeting-frame-pipeline-layout"),
                bind_group_layouts: &[&frame_bind_group_layout],
                push_constant_ranges: &[],
            });
        let frame_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("greeting-frame-pipeline"),
            layout: Some(&frame_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &frame_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<FrameVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    }],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &frame_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("greeting-frame-bind-group"),
            layout: &frame_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_uniform_buffer.as_entire_binding(),
            }],
        });
        let frame_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("greeting-frame-vertices"),
            size: 1,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("greeting-frame-indices"),
            size: 2,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let instance = GreetingScreen {
            device: device.clone(),
            queue: queue.clone(),
            _format: format,
            _cache: cache,
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
            accent_colour,
            stroke_width_dip,
            corner_radius_dip,
            size: PhysicalSize::new(0, 0),
            scale_factor: 1.0,
            text_origin: (0.0, 0.0),
            frame_pipeline,
            frame_bind_group,
            frame_uniform_buffer,
            frame_vertex_buffer,
            frame_index_buffer,
            frame_index_count: 0,
        };
        instance
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.size = new_size;
        self.scale_factor = scale_factor;
        self.write_frame_uniforms();
    }

    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) -> bool {
        if self.size.width == 0 || self.size.height == 0 {
            return false;
        }

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

        if self.frame_index_count > 0 {
            pass.set_pipeline(&self.frame_pipeline);
            pass.set_bind_group(0, &self.frame_bind_group, &[]);
            pass.set_vertex_buffer(0, self.frame_vertex_buffer.slice(..));
            pass.set_index_buffer(self.frame_index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.frame_index_count, 0, 0..1);
        }

        if let Err(err) = self
            .text_renderer
            .render(&self.atlas, &self.viewport, &mut pass)
        {
            warn!(error = %err, "greeting_screen_draw_failed");
        }

        drop(pass);

        self.atlas.trim();
        true
    }

    pub fn after_submit(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Wait);
    }

    pub fn update_layout(&mut self) -> bool {
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
        self.rebuild_frame_geometry()
    }

    fn write_frame_uniforms(&mut self) {
        let uniforms = FrameUniforms {
            color: to_linear_array(self.accent_colour),
            screen_size: [
                self.size.width.max(1) as f32,
                self.size.height.max(1) as f32,
            ],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.frame_uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    fn rebuild_frame_geometry(&mut self) -> bool {
        if self.size.width == 0 || self.size.height == 0 {
            self.frame_index_count = 0;
            return false;
        }

        let mut geometry = VertexBuffers::<FrameVertex, u16>::new();
        let stroke_px = (self.stroke_width_dip * self.scale_factor as f32).max(0.5);
        let padding_px = (MIN_FRAME_PADDING_DIP * self.scale_factor as f32).max(stroke_px * 2.0);
        let gap_px = (stroke_px * FRAME_GAP_MULTIPLIER).max(4.0 * self.scale_factor as f32);
        let corner_radius_px = (self.corner_radius_dip * self.scale_factor as f32).max(0.0);

        let layout_bounds = compute_text_bounds(&self.text_buffer, self.text_origin);
        let (frame_width, frame_height) = if let Some(bounds) = layout_bounds {
            let width = (bounds.right - bounds.left).max(1.0) + padding_px * 2.0;
            let height = (bounds.bottom - bounds.top).max(stroke_px) + padding_px * 2.0;
            (width, height)
        } else {
            let width = (self.size.width as f32 * 0.6).max(padding_px * 2.0 + stroke_px);
            let height = (self.size.height as f32 * 0.25).max(padding_px * 2.0 + stroke_px);
            (width, height)
        };

        let max_width = self.size.width as f32;
        let max_height = self.size.height as f32;
        let frame_width = frame_width.min(max_width);
        let frame_height = frame_height.min(max_height);

        let center_x = max_width * 0.5;
        let center_y = max_height * 0.5;
        let left = (center_x - frame_width * 0.5).clamp(0.0, (max_width - frame_width).max(0.0));
        let top = (center_y - frame_height * 0.5).clamp(0.0, (max_height - frame_height).max(0.0));

        let outer_rect = Box2D::new(
            point(left, top),
            point(left + frame_width, top + frame_height),
        );
        let outer_radius = corner_radius_px + stroke_px * 0.5;
        let mut tessellator = StrokeTessellator::new();
        if let Err(err) = tessellate_rounded_rect(
            &mut tessellator,
            outer_rect,
            outer_radius,
            stroke_px,
            &mut geometry,
        ) {
            warn!(error = %err, "greeting_screen_outer_frame_tessellation_failed");
            self.frame_index_count = 0;
            return false;
        }

        let inner_offset = stroke_px + gap_px;
        if frame_width > inner_offset * 2.0 && frame_height > inner_offset * 2.0 {
            let inner_rect = Box2D::new(
                point(left + inner_offset, top + inner_offset),
                point(
                    left + frame_width - inner_offset,
                    top + frame_height - inner_offset,
                ),
            );
            let inner_radius = (corner_radius_px - gap_px * 0.5).max(0.0);
            if let Err(err) = tessellate_rounded_rect(
                &mut tessellator,
                inner_rect,
                inner_radius,
                stroke_px,
                &mut geometry,
            ) {
                warn!(error = %err, "greeting_screen_inner_frame_tessellation_failed");
            }
        }

        if geometry.indices.is_empty() || geometry.vertices.is_empty() {
            self.frame_index_count = 0;
            return true;
        }

        self.frame_index_count = geometry.indices.len() as u32;
        self.frame_vertex_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("greeting-frame-vertices"),
                    contents: bytemuck::cast_slice(&geometry.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
        self.frame_index_buffer =
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("greeting-frame-indices"),
                    contents: bytemuck::cast_slice(&geometry.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });

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

fn resolve_accent_colour(source: Option<&str>) -> LinSrgba<f32> {
    source
        .and_then(parse_hex_color)
        .unwrap_or_else(default_accent_colour)
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

fn to_linear_array(color: LinSrgba<f32>) -> [f32; 4] {
    [color.red, color.green, color.blue, color.alpha]
}

#[derive(Clone, Copy)]
struct LayoutBounds {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

fn compute_text_bounds(buffer: &Buffer, origin: (f32, f32)) -> Option<LayoutBounds> {
    let mut bounds = LayoutBounds {
        left: f32::MAX,
        right: f32::MIN,
        top: f32::MAX,
        bottom: f32::MIN,
    };
    let mut has_runs = false;

    for run in buffer.layout_runs() {
        has_runs = true;

        let mut run_min_x = f32::MAX;
        let mut run_max_x = f32::MIN;
        for glyph in run.glyphs.iter() {
            run_min_x = run_min_x.min(glyph.x);
            run_max_x = run_max_x.max(glyph.x + glyph.w);
        }

        if run_min_x.is_finite() && run_max_x.is_finite() {
            bounds.left = bounds.left.min(run_min_x + origin.0);
            bounds.right = bounds.right.max(run_max_x + origin.0);
        }

        let line_top = run.line_top + origin.1;
        let line_bottom = line_top + run.line_height;
        bounds.top = bounds.top.min(line_top);
        bounds.bottom = bounds.bottom.max(line_bottom);
    }

    if has_runs {
        if !bounds.left.is_finite() || !bounds.right.is_finite() {
            bounds.left = 0.0;
            bounds.right = 0.0;
        }
        Some(bounds)
    } else {
        None
    }
}

fn tessellate_rounded_rect(
    tessellator: &mut StrokeTessellator,
    rect: Box2D,
    radius: f32,
    stroke_width: f32,
    geometry: &mut VertexBuffers<FrameVertex, u16>,
) -> Result<(), TessellationError> {
    let width = rect.width().abs();
    let height = rect.height().abs();

    if width <= 0.0 || height <= 0.0 || stroke_width <= 0.0 {
        return Ok(());
    }

    let max_radius = 0.5 * width.min(height);
    let clamped_radius = radius.clamp(0.0, max_radius);

    let mut builder = Path::builder();
    builder.add_rounded_rectangle(&rect, &BorderRadii::new(clamped_radius), Winding::Positive);
    let path = builder.build();

    let options = StrokeOptions::default()
        .with_line_width(stroke_width)
        .with_line_join(LineJoin::Round)
        .with_line_cap(LineCap::Round);

    tessellator.tessellate_path(
        &path,
        &options,
        &mut BuffersBuilder::new(geometry, FrameVertexCtor),
    )
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

fn default_accent_colour() -> LinSrgba<f32> {
    parse_hex_color("#38BDF8").unwrap()
}
