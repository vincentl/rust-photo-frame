use std::f32::consts::TAU;

use bytemuck::{Pod, Zeroable};
use lyon::geom::{Angle, ArcFlags};
use lyon::math::{point, vector};
use lyon::path::Path;
use lyon::path::builder::SvgPathBuilder;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, StrokeOptions, StrokeTessellator,
    StrokeVertex, VertexBuffers, FillRule,
};
use wgpu::util::DeviceExt;
use std::env;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::debug;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BladeVertex {
    position: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BladeInstance {
    rotation: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BladeUniforms {
    scale: [f32; 2],
    opacity: f32,
    _pad0: f32,
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniforms {
    screen_size: [f32; 2],
    stage: u32,
    _pad0: u32,
    current_dest: [f32; 4],
    next_dest: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrisStage {
    Closing,
    Opening,
}

impl IrisStage {
    fn as_u32(self) -> u32 {
        match self {
            Self::Closing => 0,
            Self::Opening => 1,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IrisDrawParams<'a> {
    pub screen_size: [f32; 2],
    pub blades: u32,
    pub closeness: f32,
    pub tolerance: f32,
    pub stroke_width: f32,
    pub rotation: f32,
    pub fill_color: [f32; 4],
    pub stroke_color: [f32; 4],
    pub stage: IrisStage,
    pub current_rect: [f32; 4],
    pub next_rect: [f32; 4],
    pub current_bind: &'a wgpu::BindGroup,
    pub next_bind: &'a wgpu::BindGroup,
}

#[derive(Clone, Copy)]
struct MeshKey {
    blades: u32,
    closeness: f32,
    radius: f32,
    tolerance: f32,
    stroke_width: f32,
}

struct MaskTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

pub struct IrisRenderer {
    mask_pipeline: wgpu::RenderPipeline,
    color_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    blade_uniform_buf: wgpu::Buffer,
    blade_uniform_bind: wgpu::BindGroup,
    composite_uniform_buf: wgpu::Buffer,
    composite_uniform_bind: wgpu::BindGroup,
    mask_sampler: wgpu::Sampler,
    mask_bind_layout: wgpu::BindGroupLayout,
    mask_bind: Option<wgpu::BindGroup>,
    mask_target: Option<MaskTarget>,
    fill_vertex: Option<wgpu::Buffer>,
    fill_index: Option<wgpu::Buffer>,
    fill_index_count: u32,
    stroke_vertex: Option<wgpu::Buffer>,
    stroke_index: Option<wgpu::Buffer>,
    stroke_index_count: u32,
    instance_buf: Option<wgpu::Buffer>,
    instance_count: u32,
    last_mesh: Option<MeshKey>,
    diag_frames: u32,
    only_interesting: bool,
    diag_file: Option<PathBuf>,
    diag_range: Option<(f32, f32)>,
    diag_cooldown: Duration,
    last_diag: Option<Instant>,
    long_edge_factor: f32,
    large_area_fraction: f32,
}

impl IrisRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        img_bind_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let diag_frames = env::var("PHOTOFRAME_IRIS_DIAG_FRAMES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        let only_interesting = env::var("PHOTOFRAME_IRIS_INTERESTING_ONLY")
            .map(|s| matches!(s.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        let diag_file = env::var("PHOTOFRAME_IRIS_DIAG_FILE").ok().map(PathBuf::from);
        let diag_range = env::var("PHOTOFRAME_IRIS_DIAG_CLOSENESS")
            .ok()
            .and_then(|s| {
                let parts: Vec<_> = s.split(|c| c == '-' || c == ',').collect();
                if parts.len() == 2 {
                    let lo = parts[0].trim().parse::<f32>().ok()?;
                    let hi = parts[1].trim().parse::<f32>().ok()?;
                    Some((lo.min(hi), lo.max(hi)))
                } else {
                    None
                }
            });
        let diag_cooldown = env::var("PHOTOFRAME_IRIS_DIAG_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ms| Duration::from_millis(ms))
            .unwrap_or(Duration::from_millis(200));
        let long_edge_factor = env::var("PHOTOFRAME_IRIS_LONG_EDGE_FACTOR")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.25);
        let large_area_fraction = env::var("PHOTOFRAME_IRIS_LARGE_AREA_FRACTION")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.2);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iris-tessellation"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "../tasks/shaders/iris_tess.wgsl"
            ))),
        });

        let blade_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris-blade-uniforms"),
            size: std::mem::size_of::<BladeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blade_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iris-blade-uniform-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let blade_uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris-blade-uniform-bind"),
            layout: &blade_bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: blade_uniform_buf.as_entire_binding(),
            }],
        });

        let composite_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris-composite-uniforms"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("iris-composite-uniform-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let composite_uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris-composite-uniform-bind"),
            layout: &composite_bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: composite_uniform_buf.as_entire_binding(),
            }],
        });

        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris-mask-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mask_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iris-mask-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let mask_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris-mask-pipeline-layout"),
            bind_group_layouts: &[&blade_bind_layout],
            push_constant_ranges: &[],
        });
        let mask_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iris-mask-pipeline"),
            layout: Some(&mask_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_blade"),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<BladeVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<BladeInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![1 => Float32x2],
                    },
                ],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mask"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });

        let color_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("iris-color-pipeline-layout"),
                bind_group_layouts: &[&blade_bind_layout],
                push_constant_ranges: &[],
            });
        let color_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iris-color-pipeline"),
            layout: Some(&color_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_blade"),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<BladeVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<BladeInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![1 => Float32x2],
                    },
                ],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_color"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });

        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("iris-composite-pipeline-layout"),
                bind_group_layouts: &[
                    &composite_bind_layout,
                    img_bind_layout,
                    img_bind_layout,
                    &mask_bind_layout,
                ],
                push_constant_ranges: &[],
            });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iris-composite-pipeline"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_fullscreen"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_composite"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });

        Self {
            mask_pipeline,
            color_pipeline,
            composite_pipeline,
            blade_uniform_buf,
            blade_uniform_bind,
            composite_uniform_buf,
            composite_uniform_bind,
            mask_sampler,
            mask_bind_layout,
            mask_bind: None,
            mask_target: None,
            fill_vertex: None,
            fill_index: None,
            fill_index_count: 0,
            stroke_vertex: None,
            stroke_index: None,
            stroke_index_count: 0,
            instance_buf: None,
            instance_count: 0,
            last_mesh: None,
            diag_frames,
            only_interesting,
            diag_file,
            diag_range,
            diag_cooldown,
            last_diag: None,
            long_edge_factor,
            large_area_fraction,
        }
    }

    fn ensure_mask(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            self.mask_target = None;
            self.mask_bind = None;
            return;
        }
        let needs_recreate = self
            .mask_target
            .as_ref()
            .map(|mask| mask.size != (width, height))
            .unwrap_or(true);
        if !needs_recreate {
            return;
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris-mask-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris-mask-bind"),
            layout: &self.mask_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
            ],
        });

        self.mask_target = Some(MaskTarget {
            _texture: texture,
            view,
            size: (width, height),
        });
        self.mask_bind = Some(bind);
    }

    fn rebuild_mesh(&mut self, device: &wgpu::Device, params: MeshKey) {
        let MeshKey {
            blades,
            closeness,
            radius,
            tolerance,
            stroke_width,
        } = params;
        let min_closeness = 1e-4;
        if closeness <= min_closeness || blades < 3 {
            self.fill_index_count = 0;
            self.stroke_index_count = 0;
            self.last_mesh = Some(params);
            return;
        }

        let (path, clamped) = build_blade_path(blades as usize, closeness, radius);
        let (fill, stroke) = tessellate_path(&path, tolerance, stroke_width.max(0.0));

        // Compute diagnostics
        let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
        let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut any_nan = false;
        let mut any_inf = false;
        for v in &fill.vertices {
            let x = v[0];
            let y = v[1];
            if !x.is_finite() || !y.is_finite() {
                any_nan |= x.is_nan() || y.is_nan();
                any_inf |= x.is_infinite() || y.is_infinite();
                continue;
            }
            if x < min_x {
                min_x = x;
            }
            if y < min_y {
                min_y = y;
            }
            if x > max_x {
                max_x = x;
            }
            if y > max_y {
                max_y = y;
            }
        }
        let max_index_fill = fill.indices.iter().copied().max().unwrap_or(0);
        let max_index_stroke = stroke.indices.iter().copied().max().unwrap_or(0);
        let oob_fill = !fill.indices.is_empty() && (max_index_fill as usize >= fill.vertices.len());
        let oob_stroke = !stroke.indices.is_empty() && (max_index_stroke as usize >= stroke.vertices.len());

        // One-frame heavy diagnostics: longest edge and largest triangle area
        let mut max_edge_len: f32 = 0.0;
        let mut max_edge_tri: (u32, u32, u32) = (0, 0, 0);
        let mut max_area: f32 = 0.0;
        let mut max_area_tri: (u32, u32, u32) = (0, 0, 0);
        let mut tri_mod_mismatch = false;
        if fill.indices.len() % 3 != 0 { tri_mod_mismatch = true; }
        for tri in (0..fill.indices.len()).step_by(3) {
            if tri + 2 >= fill.indices.len() { break; }
            let i0 = fill.indices[tri] as usize;
            let i1 = fill.indices[tri + 1] as usize;
            let i2 = fill.indices[tri + 2] as usize;
            if i0 >= fill.vertices.len() || i1 >= fill.vertices.len() || i2 >= fill.vertices.len() {
                continue;
            }
            let p0 = fill.vertices[i0];
            let p1 = fill.vertices[i1];
            let p2 = fill.vertices[i2];
            let e01 = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt();
            let e12 = ((p2[0] - p1[0]).powi(2) + (p2[1] - p1[1]).powi(2)).sqrt();
            let e20 = ((p0[0] - p2[0]).powi(2) + (p0[1] - p2[1]).powi(2)).sqrt();
            let edge_max = e01.max(e12).max(e20);
            if edge_max > max_edge_len {
                max_edge_len = edge_max;
                max_edge_tri = (fill.indices[tri], fill.indices[tri + 1], fill.indices[tri + 2]);
            }
            let area = ((p1[0] - p0[0]) * (p2[1] - p0[1]) - (p2[0] - p0[0]) * (p1[1] - p0[1])).abs() * 0.5;
            if area > max_area {
                max_area = area;
                max_area_tri = (fill.indices[tri], fill.indices[tri + 1], fill.indices[tri + 2]);
            }
        }

        // Heuristics for interesting events
        let long_edge_flag = max_edge_len > radius * self.long_edge_factor; // adjustable
        let large_area_flag = max_area > (radius * radius) * self.large_area_fraction; // adjustable
        let near_flip = closeness > 0.48 && closeness < 0.52;
        let interesting = any_nan
            || any_inf
            || oob_fill
            || oob_stroke
            || tri_mod_mismatch
            || long_edge_flag
            || large_area_flag
            || near_flip
            || clamped;

        // External triggers
        let mut diag_requested = self.diag_frames > 0;
        if let Some((lo, hi)) = self.diag_range {
            if closeness >= lo && closeness <= hi {
                diag_requested = true;
            }
        }
        if let Some(ref path) = self.diag_file {
            if std::fs::metadata(path).is_ok() {
                diag_requested = true;
            }
        }

        // Rate limit heavy logs unless it's interesting
        let now = Instant::now();
        let within_cooldown = self
            .last_diag
            .map(|t| now.saturating_duration_since(t) < self.diag_cooldown)
            .unwrap_or(false);

        let should_log = (!self.only_interesting || interesting || diag_requested)
            && (!within_cooldown || interesting);
        if should_log {
            debug!(
                blades,
                closeness,
                radius,
                tolerance,
                stroke_width,
                fill_vertices = fill.vertices.len(),
                fill_indices = fill.indices.len(),
                stroke_vertices = stroke.vertices.len(),
                stroke_indices = stroke.indices.len(),
                max_index_fill,
                max_index_stroke,
                oob_fill,
                oob_stroke,
                bbox_min_x = min_x,
                bbox_min_y = min_y,
                bbox_max_x = max_x,
                bbox_max_y = max_y,
                acos_clamped = clamped,
                any_nan,
                any_inf,
                tri_mod_mismatch,
                long_edge = max_edge_len,
                long_edge_tri = ?max_edge_tri,
                large_area = max_area,
                large_area_tri = ?max_area_tri,
                near_flip,
                interesting,
                "iris_mesh_rebuilt",
            );
            self.last_diag = Some(now);
            if interesting {
                debug!(
                    closeness,
                    long_edge = max_edge_len,
                    long_edge_tri = ?max_edge_tri,
                    large_area = max_area,
                    large_area_tri = ?max_area_tri,
                    oob_fill,
                    oob_stroke,
                    tri_mod_mismatch,
                    acos_clamped = clamped,
                    "iris_alert",
                );
            }
        }
        if self.diag_frames > 0 {
            self.diag_frames = self.diag_frames.saturating_sub(1);
        }
        if oob_fill || oob_stroke {
            // If we ever see this, skip drawing to avoid undefined behavior.
            self.fill_index_count = 0;
            self.stroke_index_count = 0;
        }

        if fill.vertices.is_empty() || fill.indices.is_empty() {
            self.fill_index_count = 0;
            self.fill_vertex = None;
            self.fill_index = None;
        } else {
            let fill_vertices: Vec<BladeVertex> = fill
                .vertices
                .iter()
                .map(|pos| BladeVertex { position: *pos })
                .collect();
            let fill_indices = fill.indices;
            self.fill_vertex = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("iris-fill-vertices"),
                    contents: bytemuck::cast_slice(&fill_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
            );
            self.fill_index = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("iris-fill-indices"),
                    contents: bytemuck::cast_slice(&fill_indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
            );
            self.fill_index_count = fill_indices.len() as u32;
        }

        if stroke_width <= 0.0 || stroke.vertices.is_empty() || stroke.indices.is_empty() {
            self.stroke_index_count = 0;
            self.stroke_vertex = None;
            self.stroke_index = None;
        } else {
            let stroke_vertices: Vec<BladeVertex> = stroke
                .vertices
                .iter()
                .map(|pos| BladeVertex { position: *pos })
                .collect();
            let stroke_indices = stroke.indices;
            self.stroke_vertex = Some(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("iris-stroke-vertices"),
                    contents: bytemuck::cast_slice(&stroke_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                },
            ));
            self.stroke_index = Some(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("iris-stroke-indices"),
                    contents: bytemuck::cast_slice(&stroke_indices),
                    usage: wgpu::BufferUsages::INDEX,
                },
            ));
            self.stroke_index_count = stroke_indices.len() as u32;
        }

        self.last_mesh = Some(params);
    }

    fn update_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        blades: u32,
        rotation: f32,
    ) {
        if blades == 0 {
            self.instance_count = 0;
            self.instance_buf = None;
            return;
        }
        let mut data = Vec::with_capacity(blades as usize);
        let step = TAU / blades as f32;
        for i in 0..blades {
            let angle = rotation + step * (i as f32);
            data.push(BladeInstance {
                rotation: [angle.cos(), angle.sin()],
                _pad: [0.0; 2],
            });
        }
        let bytes = bytemuck::cast_slice(&data);
        let required = bytes.len() as u64;
        match &self.instance_buf {
            Some(buf) if buf.size() >= required => {
                queue.write_buffer(buf, 0, bytes);
            }
            _ => {
                let buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris-instance-buffer"),
                    size: required,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&buf, 0, bytes);
                self.instance_buf = Some(buf);
            }
        }
        self.instance_count = blades;
    }

    fn write_blade_uniforms(
        &self,
        queue: &wgpu::Queue,
        screen_size: [f32; 2],
        opacity: f32,
        color: [f32; 4],
    ) {
        let scale = if screen_size[0] > 0.0 && screen_size[1] > 0.0 {
            [2.0 / screen_size[0], 2.0 / screen_size[1]]
        } else {
            [0.0, 0.0]
        };
        let uniforms = BladeUniforms {
            scale,
            opacity,
            _pad0: 0.0,
            color,
        };
        queue.write_buffer(&self.blade_uniform_buf, 0, bytemuck::bytes_of(&uniforms));
    }

    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        params: IrisDrawParams<'_>,
    ) -> bool {
        let width = params.screen_size[0].max(1.0);
        let height = params.screen_size[1].max(1.0);
        self.ensure_mask(device, width as u32, height as u32);
        if self.mask_target.is_none() || self.mask_bind.is_none() {
            return false;
        }

        // Oversize radius so the iris covers the entire screen; top/bottom may clip.
        // Use half the screen diagonal which guarantees coverage in all rotations.
        let radius = ((params.screen_size[0]).powi(2) + (params.screen_size[1]).powi(2)).sqrt() * 0.5;
        let mesh_key = MeshKey {
            blades: params.blades,
            closeness: params.closeness,
            radius,
            tolerance: params.tolerance.max(1e-3),
            stroke_width: params.stroke_width.max(0.0),
        };

        let needs_rebuild = self
            .last_mesh
            .map(|last| {
                last.blades != mesh_key.blades
                    || (last.closeness - mesh_key.closeness).abs() > 1e-4
                    || (last.radius - mesh_key.radius).abs() > 1e-2
                    || (last.tolerance - mesh_key.tolerance).abs() > 1e-4
                    || (last.stroke_width - mesh_key.stroke_width).abs() > 1e-3
            })
            .unwrap_or(true);
        if needs_rebuild {
            self.rebuild_mesh(device, mesh_key);
        }
        if params.blades == 0 {
            return false;
        }
        self.update_instances(device, queue, params.blades, params.rotation);
        if self.instance_count == 0 {
            return false;
        }
        let mask_bind = self.mask_bind.as_ref().unwrap();

        // Mask pass
        {
            debug!(
                stage = ?params.stage,
                closeness = params.closeness,
                instance_count = self.instance_count,
                screen_w = params.screen_size[0],
                screen_h = params.screen_size[1],
                mask_w = self.mask_target.as_ref().map(|m| m.size.0),
                mask_h = self.mask_target.as_ref().map(|m| m.size.1),
                "iris_draw_begin",
            );
            self.write_blade_uniforms(queue, params.screen_size, 1.0, [0.0; 4]);
            let mask_view = &self.mask_target.as_ref().unwrap().view;
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris-mask-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: mask_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            if self.fill_index_count > 0 {
                rpass.set_pipeline(&self.mask_pipeline);
                rpass.set_bind_group(0, &self.blade_uniform_bind, &[]);
                rpass.set_vertex_buffer(0, self.fill_vertex.as_ref().unwrap().slice(..));
                rpass.set_vertex_buffer(1, self.instance_buf.as_ref().unwrap().slice(..));
                rpass.set_index_buffer(
                    self.fill_index.as_ref().unwrap().slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.fill_index_count, 0, 0..self.instance_count);
            }
        }

        // Composite pass
        let composite = CompositeUniforms {
            screen_size: params.screen_size,
            stage: params.stage.as_u32(),
            _pad0: 0,
            current_dest: params.current_rect,
            next_dest: params.next_rect,
        };
        queue.write_buffer(
            &self.composite_uniform_buf,
            0,
            bytemuck::bytes_of(&composite),
        );
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris-composite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            rpass.set_pipeline(&self.composite_pipeline);
            rpass.set_bind_group(0, &self.composite_uniform_bind, &[]);
            rpass.set_bind_group(1, params.current_bind, &[]);
            rpass.set_bind_group(2, params.next_bind, &[]);
            rpass.set_bind_group(3, mask_bind, &[]);
            rpass.draw(0..6, 0..1);
        }

        if params.closeness <= 1e-4 {
            return true;
        }

        // Overlay pass (fill + stroke)
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris-overlay-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            rpass.set_pipeline(&self.color_pipeline);
            rpass.set_bind_group(0, &self.blade_uniform_bind, &[]);
            if self.fill_index_count > 0 {
                self.write_blade_uniforms(
                    queue,
                    params.screen_size,
                    1.0,
                    params.fill_color,
                );
                rpass.set_vertex_buffer(0, self.fill_vertex.as_ref().unwrap().slice(..));
                rpass.set_vertex_buffer(1, self.instance_buf.as_ref().unwrap().slice(..));
                rpass.set_index_buffer(
                    self.fill_index.as_ref().unwrap().slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.fill_index_count, 0, 0..self.instance_count);
            }
            if self.stroke_index_count > 0 {
                self.write_blade_uniforms(
                    queue,
                    params.screen_size,
                    1.0,
                    params.stroke_color,
                );
                rpass.set_vertex_buffer(0, self.stroke_vertex.as_ref().unwrap().slice(..));
                rpass.set_vertex_buffer(1, self.instance_buf.as_ref().unwrap().slice(..));
                rpass.set_index_buffer(
                    self.stroke_index.as_ref().unwrap().slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                rpass.draw_indexed(0..self.stroke_index_count, 0, 0..self.instance_count);
            }
        }

        debug!(
            stage = ?params.stage,
            closeness = params.closeness,
            rendered = true,
            "iris_draw_end",
        );

        true
    }
}

fn build_blade_path(count: usize, closeness: f32, radius: f32) -> (Path, bool) {
    let (p1, mid, tip, clamped) = blade_points(count, closeness, radius);
    let mut builder = Path::builder().with_svg();
    let radii = vector(radius, radius);
    let zero = Angle::zero();
    builder.move_to(point(p1.0, p1.1));
    builder.arc_to(
        radii,
        zero,
        ArcFlags {
            large_arc: false,
            sweep: false,
        },
        point(mid.0, mid.1),
    );
    builder.arc_to(
        radii,
        zero,
        ArcFlags {
            large_arc: false,
            sweep: true,
        },
        point(tip.0, tip.1),
    );
    builder.arc_to(
        radii,
        zero,
        ArcFlags {
            large_arc: false,
            sweep: false,
        },
        point(p1.0, p1.1),
    );
    builder.close();
    (builder.build(), clamped)
}

fn tessellate_path(
    path: &Path,
    tolerance: f32,
    stroke_width: f32,
) -> (VertexBuffers<[f32; 2], u32>, VertexBuffers<[f32; 2], u32>) {
    let mut fill: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let fill_opts = FillOptions::tolerance(tolerance.clamp(0.05, 5.0))
        .with_fill_rule(FillRule::EvenOdd);
    FillTessellator::new()
        .tessellate_path(
            path,
            &fill_opts,
            &mut BuffersBuilder::new(&mut fill, |v: FillVertex| v.position().to_array()),
        )
        .expect("fill tessellation");

    let mut stroke: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    if stroke_width > 0.0 {
        let stroke_opts =
            StrokeOptions::tolerance(tolerance.clamp(0.05, 5.0)).with_line_width(stroke_width);
        StrokeTessellator::new()
            .tessellate_path(
                path,
                &stroke_opts,
                &mut BuffersBuilder::new(&mut stroke, |v: StrokeVertex| v.position().to_array()),
            )
            .expect("stroke tessellation");
    }

    (fill, stroke)
}

fn blade_points(
    count: usize,
    closeness: f32,
    radius: f32,
) -> ((f32, f32), (f32, f32), (f32, f32), bool) {
    let count = count.max(1) as f32;
    let step = std::f32::consts::PI * (0.5 + 2.0 / count);
    let p1x = step.cos() * radius;
    let p1y = step.sin() * radius;
    let val = closeness.clamp(0.0, 1.2);
    let (sinv, cosv) = (-val).sin_cos();
    let c1x = p1x - cosv * p1x - sinv * p1y;
    let c1y = p1y - cosv * p1y + sinv * p1x;
    let dx = -sinv * radius - c1x;
    let dy = radius - cosv * radius - c1y;
    let dc = (dx * dx + dy * dy).sqrt();
    // Guard against numerical drift that can push the ratio just outside [-1, 1]
    let raw = dc / (2.0 * radius);
    let cos_arg = raw.clamp(-1.0, 1.0);
    let a = dy.atan2(dx) - cos_arg.acos();
    let tipx = c1x + a.cos() * radius;
    let tipy = c1y + a.sin() * radius;
    let clamped = raw < -1.0 || raw > 1.0;
    ((p1x, p1y), (0.0, radius), (tipx, tipy), clamped)
}
