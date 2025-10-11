use std::path::PathBuf;
use std::str::FromStr;

use bytemuck::{Pod, Zeroable};
use fontdb::{Database, Family, Query};
use glyphon::cosmic_text::Align;
use glyphon::{
    Attrs, Buffer, Cache, Color, FamilyOwned, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use palette::{LinSrgba, Srgb, Srgba};
use tracing::warn;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

use crate::config::ScreenMessageConfig;

const FRAME_SHADER: wgpu::ShaderModuleDescriptor<'static> =
    wgpu::include_wgsl!("shaders/greeting_frame.wgsl");

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    size_outer_inner: [f32; 4],
    gap_radius: [f32; 4],
    color: [f32; 4],
}

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
    accent_colour: LinSrgba<f32>,
    stroke_width_dip: f32,
    corner_radius_dip: f32,
    size: PhysicalSize<u32>,
    scale_factor: f64,
    text_origin: (f32, f32),
    frame_uniform: FrameUniform,
    frame_buffer: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
    frame_pipeline: wgpu::RenderPipeline,
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
        let accent_colour = resolve_accent_colour(screen.colors.accent.as_deref());
        let stroke_width_dip = screen.effective_stroke_width_dip();
        let corner_radius_dip = screen.effective_corner_radius_dip(stroke_width_dip);

        let frame_uniform = FrameUniform {
            size_outer_inner: [0.0, 0.0, 0.0, 0.0],
            gap_radius: [0.0, 0.0, 0.0, 0.0],
            color: [
                accent_colour.red,
                accent_colour.green,
                accent_colour.blue,
                accent_colour.alpha,
            ],
        };
        let frame_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("greeting-frame-uniform"),
            contents: bytemuck::bytes_of(&frame_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let frame_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("greeting-frame-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("greeting-frame-bind-group"),
            layout: &frame_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buffer.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(FRAME_SHADER);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("greeting-frame-pipeline-layout"),
            bind_group_layouts: &[&frame_bind_group_layout],
            push_constant_ranges: &[],
        });
        let frame_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                    blend: None,
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

        GreetingScreen {
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
            accent_colour,
            stroke_width_dip,
            corner_radius_dip,
            size: PhysicalSize::new(0, 0),
            scale_factor: 1.0,
            text_origin: (0.0, 0.0),
            frame_uniform,
            frame_buffer,
            frame_bind_group,
            frame_pipeline,
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.size = new_size;
        self.scale_factor = scale_factor;
        self.update_frame_uniform();
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

            pass.set_pipeline(&self.frame_pipeline);
            pass.set_bind_group(0, &self.frame_bind_group, &[]);
            pass.draw(0..3, 0..1);

            if let Err(err) = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
            {
                warn!(error = %err, "greeting_screen_draw_failed");
            }
        }

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
        true
    }

    fn update_frame_uniform(&mut self) {
        self.frame_uniform.color = [
            self.accent_colour.red,
            self.accent_colour.green,
            self.accent_colour.blue,
            self.accent_colour.alpha,
        ];

        let width = self.size.width as f32;
        let height = self.size.height as f32;

        if width <= 0.0 || height <= 0.0 {
            self.frame_uniform.size_outer_inner = [width, height, 0.0, 0.0];
            self.frame_uniform.gap_radius = [0.0, 0.0, 0.0, 0.0];
        } else {
            let scale = self.scale_factor as f32;
            let mut outer_stroke = (self.stroke_width_dip * scale).clamp(0.5, width.min(height));
            let mut gap = (outer_stroke * 0.5).max(0.5);
            let mut inner_stroke = (outer_stroke * 0.6).max(0.5);
            let mut corner_radius = (self.corner_radius_dip * scale).max(0.0);

            let half_min = 0.5 * width.min(height);
            if half_min > 1.0 {
                let total = outer_stroke + gap + inner_stroke;
                if total >= half_min {
                    let limit = (half_min - 1.0).max(0.5);
                    if total > 0.0 {
                        let factor = (limit / total).clamp(0.1, 1.0);
                        outer_stroke *= factor;
                        gap *= factor;
                        inner_stroke *= factor;
                    }
                }
            } else {
                outer_stroke = half_min.max(0.0);
                gap = 0.0;
                inner_stroke = 0.0;
            }

            let max_corner = half_min.max(0.0);
            if corner_radius > max_corner {
                corner_radius = max_corner;
            }

            self.frame_uniform.size_outer_inner = [width, height, outer_stroke, inner_stroke];
            self.frame_uniform.gap_radius = [gap, corner_radius, 0.0, 0.0];
        }

        self.queue.write_buffer(
            &self.frame_buffer,
            0,
            bytemuck::bytes_of(&self.frame_uniform),
        );
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
