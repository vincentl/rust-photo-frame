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
use crate::gpu::debug_overlay;

/// Lightweight greeting/sleep screen renderer: clears the surface to the
/// configured background colour and renders centred text using `glyphon`.
pub struct GreetingScreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
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
    size: PhysicalSize<u32>,
    text_origin: (f32, f32),
    stroke_dip: f32,
    corner_radius_dip: f32,
    scale_factor: f64,
    padding_px: f32,
    frame_renderer: FrameRenderer,
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

        let stroke_dip = screen.effective_stroke_width_dip();
        let corner_radius_dip = screen.effective_corner_radius_dip(stroke_dip);

        let background = resolve_background_colour(screen.colors.background.as_deref());
        let font_colour = resolve_font_colour(screen.colors.font.as_deref());
        let accent_colour = resolve_accent_colour(screen.colors.accent.as_deref());

        let frame_renderer = FrameRenderer::new(device, format);

        let mut instance = GreetingScreen {
            device: device.clone(),
            queue: queue.clone(),
            viewport,
            atlas,
            text_renderer,
            text_buffer,
            font_system,
            swash_cache,
            font_family,
            message: String::new(),
            background,
            font_colour,
            accent_colour,
            size: PhysicalSize::new(0, 0),
            text_origin: (0.0, 0.0),
            stroke_dip,
            corner_radius_dip,
            scale_factor: 1.0,
            padding_px: 0.0,
            frame_renderer,
        };
        instance.recompute_padding();
        instance
    }

    pub fn set_message(&mut self, message: impl Into<String>) -> bool {
        let message = message.into();
        if self.message == message {
            return false;
        }
        self.message = message;
        true
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.size = new_size;
        self.scale_factor = scale_factor;
        self.recompute_padding();
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

        debug_overlay::render(
            encoder,
            target_view,
            "greeting-background",
            to_wgpu_color(self.background),
            None::<fn(&mut wgpu::RenderPass<'_>)>,
        );

        self.frame_renderer.render(encoder, target_view);

        let mut render_error = None;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("greeting-text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            if let Err(err) = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
            {
                render_error = Some(err);
            }
        }

        if let Some(err) = render_error {
            warn!(error = %err, "greeting_screen_draw_failed");
        }

        self.atlas.trim();
        true
    }

    pub fn after_submit(&mut self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    pub fn update_layout(&mut self) -> bool {
        if self.size.width == 0 || self.size.height == 0 {
            return false;
        }

        let font_size = compute_font_size(&self.message, self.size);
        let metrics = Metrics::new(font_size, font_size * 1.2);
        let available_width = (self.size.width as f32 - 2.0 * self.padding_px).max(1.0);
        let available_height = (self.size.height as f32 - 2.0 * self.padding_px).max(1.0);
        self.text_buffer.set_metrics_and_size(
            &mut self.font_system,
            metrics,
            Some(available_width),
            Some(available_height),
        );

        let attrs = Attrs::new().family(self.font_family.as_family());
        self.text_buffer.set_text(
            &mut self.font_system,
            &self.message,
            &attrs,
            Shaping::Advanced,
            None,
        );
        apply_center_alignment(&mut self.text_buffer);
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        self.text_origin = compute_text_origin(&self.text_buffer, self.size, self.padding_px);
        true
    }

    fn recompute_padding(&mut self) {
        let scale = self.scale_factor.max(0.0) as f32;
        let stroke_px = (self.stroke_dip * scale).max(0.0);
        let corner_px = (self.corner_radius_dip * scale).max(0.0);
        let inner_inset = stroke_px * 7.0;
        self.padding_px = inner_inset.max(corner_px * 0.5);
        self.frame_renderer.update(
            &self.queue,
            self.size,
            stroke_px,
            corner_px,
            self.accent_colour,
            self.background,
        );
    }
}

#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct FrameUniforms {
    resolution: [f32; 2],
    _pad0: [f32; 2],
    insets: [f32; 4],
    radii: [f32; 4],
    accent: [f32; 4],
    background: [f32; 4],
}

impl Default for FrameUniforms {
    fn default() -> Self {
        Self {
            resolution: [0.0, 0.0],
            _pad0: [0.0, 0.0],
            insets: [0.0; 4],
            radii: [0.0; 4],
            accent: [0.0; 4],
            background: [0.0; 4],
        }
    }
}

struct FrameRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    uniforms: FrameUniforms,
}

impl FrameRenderer {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("greeting-frame-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("greeting_frame.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("greeting-frame-uniforms"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("greeting-frame-bind-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<FrameUniforms>() as u64
                    ),
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("greeting-frame-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("greeting-frame-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("greeting-frame-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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

        Self {
            pipeline,
            bind_group,
            uniform_buffer,
            uniforms: FrameUniforms::default(),
        }
    }

    fn update(
        &mut self,
        queue: &wgpu::Queue,
        size: PhysicalSize<u32>,
        stroke_px: f32,
        corner_radius_px: f32,
        accent: LinSrgba<f32>,
        background: LinSrgba<f32>,
    ) {
        if size.width == 0 || size.height == 0 {
            self.uniforms = FrameUniforms::default();
            queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));
            return;
        }

        let width = size.width as f32;
        let height = size.height as f32;

        let outer_inset = stroke_px * 2.0;
        let outer_inner_inset = outer_inset + stroke_px * 2.0;
        let inner_outer_inset = outer_inner_inset + stroke_px * 2.0;
        let inner_inner_inset = inner_outer_inset + stroke_px;

        let uniforms = FrameUniforms {
            resolution: [width, height],
            _pad0: [0.0, 0.0],
            insets: [
                outer_inset,
                outer_inner_inset,
                inner_outer_inset,
                inner_inner_inset,
            ],
            radii: [
                (corner_radius_px - outer_inset).max(0.0),
                (corner_radius_px - outer_inner_inset).max(0.0),
                (corner_radius_px - inner_outer_inset).max(0.0),
                (corner_radius_px - inner_inner_inset).max(0.0),
            ],
            accent: linear_color_to_array(accent),
            background: linear_color_to_array(background),
        };

        self.uniforms = uniforms;
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));
    }

    fn render(&self, encoder: &mut wgpu::CommandEncoder, target_view: &wgpu::TextureView) {
        if self.uniforms.resolution[0] <= 0.0 || self.uniforms.resolution[1] <= 0.0 {
            return;
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("greeting-frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
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

fn compute_text_origin(buffer: &Buffer, size: PhysicalSize<u32>, padding: f32) -> (f32, f32) {
    let mut min_top = f32::MAX;
    let mut max_bottom = f32::MIN;
    let mut has_runs = false;

    for run in buffer.layout_runs() {
        has_runs = true;
        min_top = min_top.min(run.line_top);
        max_bottom = max_bottom.max(run.line_top + run.line_height);
    }

    if !has_runs {
        return (
            padding.max(0.0),
            (size.height as f32 * 0.5).max(padding.max(0.0)),
        );
    }

    let text_height = (max_bottom - min_top).max(0.0);
    let container_height = (size.height as f32 - 2.0 * padding).max(0.0);
    let centered_offset = ((container_height - text_height) * 0.5).max(0.0);
    let top_offset = padding + centered_offset - min_top;

    (padding.max(0.0), top_offset.max(padding.max(0.0)))
}

fn to_text_color(color: LinSrgba<f32>) -> Color {
    let srgb: Srgba<f32> = Srgba::from_linear(color);
    let srgb_u8: Srgba<u8> = srgb.into_format();
    Color::rgba(srgb_u8.red, srgb_u8.green, srgb_u8.blue, srgb_u8.alpha)
}

fn linear_color_to_array(color: LinSrgba<f32>) -> [f32; 4] {
    [color.red, color.green, color.blue, color.alpha]
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
