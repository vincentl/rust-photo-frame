use bytemuck::{Pod, Zeroable};
use lyon::math::{Box2D, point};
use lyon::path::Path;
use lyon::path::builder::{BorderRadii, PathBuilder};
use lyon::tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use tracing::warn;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameVertex {
    position: [f32; 2],
}

impl FrameVertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<FrameVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            }],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    color: [f32; 4],
}

pub struct FrameRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertex_buffer: Option<wgpu::Buffer>,
    index_buffer: Option<wgpu::Buffer>,
    index_count: u32,
}

impl FrameRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("greeting-frame-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("frame.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                entry_point: Some("vs"),
                buffers: &[FrameVertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("greeting-frame-uniform"),
            contents: bytemuck::bytes_of(&FrameUniform { color: [0.0; 4] }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("greeting-frame-bind-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        Self {
            pipeline,
            uniform_buffer,
            bind_group,
            vertex_buffer: None,
            index_buffer: None,
            index_count: 0,
        }
    }

    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: FrameUpdateParams,
    ) {
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&FrameUniform {
                color: params.color,
            }),
        );

        match build_geometry(&params) {
            Some(geometry) => {
                let vertex_data: Vec<FrameVertex> = geometry
                    .vertices
                    .into_iter()
                    .map(|position| FrameVertex { position })
                    .collect();

                if vertex_data.is_empty() || geometry.indices.is_empty() {
                    self.vertex_buffer = None;
                    self.index_buffer = None;
                    self.index_count = 0;
                    return;
                }

                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("greeting-frame-vertices"),
                    contents: bytemuck::cast_slice(&vertex_data),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("greeting-frame-indices"),
                    contents: bytemuck::cast_slice(&geometry.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });

                self.vertex_buffer = Some(vertex_buffer);
                self.index_buffer = Some(index_buffer);
                self.index_count = geometry.indices.len() as u32;
            }
            None => {
                self.vertex_buffer = None;
                self.index_buffer = None;
                self.index_count = 0;
            }
        }
    }

    pub fn draw<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.index_count == 0 {
            return;
        }
        if let (Some(vertex_buffer), Some(index_buffer)) = (&self.vertex_buffer, &self.index_buffer)
        {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }
    }
}

pub struct FrameUpdateParams {
    pub surface_size: PhysicalSize<u32>,
    pub text_bounds: (f32, f32),
    pub line_height: f32,
    pub color: [f32; 4],
    pub style: FrameStyle,
}

fn build_geometry(params: &FrameUpdateParams) -> Option<VertexBuffers<[f32; 2], u16>> {
    let width = params.surface_size.width as f32;
    let height = params.surface_size.height as f32;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let mut buffers: VertexBuffers<[f32; 2], u16> = VertexBuffers::new();
    let mut tessellator = FillTessellator::new();

    let path = build_frame_path(params, width, height)?;

    if let Err(err) = tessellator.tessellate_path(
        &path,
        &FillOptions::default(),
        &mut BuffersBuilder::new(&mut buffers, |vertex: FillVertex| {
            vertex.position().to_array()
        }),
    ) {
        warn!(error = %err, "greeting_frame_tessellation_failed");
        return None;
    }

    let surface_width = width.max(1.0);
    let surface_height = height.max(1.0);
    for position in &mut buffers.vertices {
        let x = (position[0] / surface_width) * 2.0 - 1.0;
        let y = 1.0 - (position[1] / surface_height) * 2.0;
        *position = [x, y];
    }

    Some(buffers)
}

fn build_frame_path(
    params: &FrameUpdateParams,
    surface_width: f32,
    surface_height: f32,
) -> Option<Path> {
    let text_width = params.text_bounds.0.max(0.0);
    let text_height = params.text_bounds.1.max(params.line_height);

    let style = params.style;
    let outer_stroke = style.outer_stroke_px;
    let inner_stroke = style.inner_stroke_px;
    let gap = style.gap_px;
    let content_padding = style.content_padding_px;
    let corner_radius = style.corner_radius_px;

    let per_side = outer_stroke + inner_stroke + gap + content_padding;
    if per_side <= 0.0 {
        return None;
    }

    let total_width = text_width + 2.0 * per_side;
    let total_height = text_height + 2.0 * per_side;
    if total_width <= 0.0 || total_height <= 0.0 {
        return None;
    }

    let available_width = surface_width;
    let available_height = surface_height;

    let width_scale = (available_width - text_width).max(0.0) / (2.0 * per_side);
    let height_scale = (available_height - text_height).max(0.0) / (2.0 * per_side);
    let frame_scale = width_scale.min(height_scale).min(1.0);

    if frame_scale <= 0.0 {
        return None;
    }

    let outer_stroke = outer_stroke * frame_scale;
    let inner_stroke = inner_stroke * frame_scale;
    let gap = gap * frame_scale;
    let content_padding = content_padding * frame_scale;
    let corner_radius = corner_radius * frame_scale;

    let per_side = outer_stroke + inner_stroke + gap + content_padding;
    if per_side <= 0.0 {
        return None;
    }

    let total_half_width = text_width * 0.5 + per_side;
    let total_half_height = text_height * 0.5 + per_side;
    let total_width = total_half_width * 2.0;
    let total_height = total_half_height * 2.0;

    if total_width <= 1.0 || total_height <= 1.0 {
        return None;
    }

    let origin_x = (available_width - total_width) * 0.5;
    let origin_y = (available_height - total_height) * 0.5;

    let mut builder = Path::builder();

    let outer_rect = Rect {
        x: origin_x,
        y: origin_y,
        width: total_width,
        height: total_height,
    };

    let outer_inner_rect = outer_rect.inset(outer_stroke);
    if outer_inner_rect.width <= 0.0 || outer_inner_rect.height <= 0.0 {
        return None;
    }

    let gap_outer_rect = outer_inner_rect.inset(gap);
    if gap_outer_rect.width <= 0.0 || gap_outer_rect.height <= 0.0 {
        return None;
    }

    let inner_inner_rect = gap_outer_rect.inset(inner_stroke);
    if inner_inner_rect.width <= 0.0 || inner_inner_rect.height <= 0.0 {
        return None;
    }

    let outer_radius = corner_radius.min(total_half_width.min(total_half_height));
    let outer_inner_radius = (outer_radius - outer_stroke).max(0.0);
    let gap_outer_radius = (outer_inner_radius - gap).max(0.0);
    let inner_inner_radius = (gap_outer_radius - inner_stroke).max(0.0);

    add_ring(
        &mut builder,
        &outer_rect,
        outer_radius,
        &outer_inner_rect,
        outer_inner_radius,
    );

    add_ring(
        &mut builder,
        &gap_outer_rect,
        gap_outer_radius,
        &inner_inner_rect,
        inner_inner_radius,
    );

    Some(builder.build())
}

fn add_ring<B>(builder: &mut B, outer: &Rect, outer_radius: f32, inner: &Rect, inner_radius: f32)
where
    B: PathBuilder,
{
    let outer_box = outer.to_box2d();
    let inner_box = inner.to_box2d();
    let outer_radius = outer_radius.clamp(0.0, outer.max_corner_radius());
    let inner_radius = inner_radius.clamp(0.0, inner.max_corner_radius());

    builder.add_rounded_rectangle(
        &outer_box,
        &BorderRadii::new(outer_radius),
        lyon::path::Winding::Positive,
        &[],
    );
    builder.add_rounded_rectangle(
        &inner_box,
        &BorderRadii::new(inner_radius),
        lyon::path::Winding::Negative,
        &[],
    );
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Rect {
    fn inset(&self, amount: f32) -> Self {
        let inset = amount.max(0.0);
        Self {
            x: self.x + inset,
            y: self.y + inset,
            width: (self.width - inset * 2.0).max(0.0),
            height: (self.height - inset * 2.0).max(0.0),
        }
    }

    fn to_box2d(&self) -> Box2D {
        Box2D::new(
            point(self.x, self.y),
            point(self.x + self.width, self.y + self.height),
        )
    }

    fn max_corner_radius(&self) -> f32 {
        self.width.min(self.height) * 0.5
    }
}

#[derive(Clone, Copy)]
pub struct FrameStyle {
    pub outer_stroke_px: f32,
    pub inner_stroke_px: f32,
    pub gap_px: f32,
    pub content_padding_px: f32,
    pub corner_radius_px: f32,
}
