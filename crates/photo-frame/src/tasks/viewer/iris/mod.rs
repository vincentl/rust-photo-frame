mod geometry;

use geometry::{IrisStrokeBuffers, Params, rebuild_buffers};

#[derive(Clone, Debug, PartialEq)]
pub struct IrisConfig {
    pub enabled: bool,
    pub petal_count: u32,
    pub radius: f32,
    pub stroke_px: f32,
    pub segments_per_90deg: u32,
    pub segments_per_cubic: u32,
    pub color_rgba: [f32; 4],
    pub value: f32,
    pub rotation: f32,
}

impl IrisConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            petal_count: 0,
            radius: 1.0,
            stroke_px: 0.0,
            segments_per_90deg: 1,
            segments_per_cubic: 1,
            color_rgba: [0.0, 0.0, 0.0, 1.0],
            value: 0.0,
            rotation: 0.0,
        }
    }
}

pub struct IrisRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    bind_group: Option<wgpu::BindGroup>,
    buffers: Option<IrisStrokeBuffers>,
    vertex_count: u32,
    instance_count: u32,
    last_params: Option<Params>,
    last_cfg: Option<IrisConfig>,
    last_viewport: Option<[f32; 2]>,
}

impl IrisRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iris-stroke-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("iris-stroke-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "../../shaders/iris_stroke.wgsl"
            ))),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris-stroke-pipeline-layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iris-stroke-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
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
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            layout,
            bind_group: None,
            buffers: None,
            vertex_count: 0,
            instance_count: 0,
            last_params: None,
            last_cfg: None,
            last_viewport: None,
        }
    }

    pub fn resize(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: [f32; 2],
        cfg: &IrisConfig,
    ) {
        self.last_viewport = Some(viewport);
        self.last_cfg = if cfg.enabled { Some(cfg.clone()) } else { None };
        if let (Some(buffers), Some(params)) = (self.buffers.as_ref(), self.last_params.as_mut()) {
            params.viewport_px = viewport;
            queue.write_buffer(&buffers.params, 0, bytemuck::bytes_of(params));
        }
    }

    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        viewport: [f32; 2],
        cfg: &IrisConfig,
    ) {
        if !cfg.enabled || cfg.petal_count == 0 || cfg.stroke_px <= 0.0 {
            self.disable();
            return;
        }

        let (buffers, vertex_count, instance_count, params) =
            rebuild_buffers(device, cfg, viewport);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris-stroke-bind-group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffers.cubics.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffers.params.as_entire_binding(),
                },
            ],
        });

        queue.write_buffer(&buffers.params, 0, bytemuck::bytes_of(&params));

        self.bind_group = Some(bind_group);
        self.buffers = Some(buffers);
        self.vertex_count = vertex_count.min(u32::MAX as usize) as u32;
        self.instance_count = instance_count;
        self.last_params = Some(params);
        self.last_cfg = Some(cfg.clone());
        self.last_viewport = Some(viewport);
    }

    pub fn disable(&mut self) {
        self.bind_group = None;
        self.buffers = None;
        self.vertex_count = 0;
        self.instance_count = 0;
        self.last_params = None;
        self.last_cfg = None;
        self.last_viewport = None;
    }

    pub fn draw<'a>(&'a self, rpass: &mut wgpu::RenderPass<'a>) {
        if self.vertex_count == 0 || self.instance_count == 0 {
            return;
        }
        if let Some(bind) = &self.bind_group {
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, bind, &[]);
            rpass.draw(0..self.vertex_count, 0..self.instance_count);
        }
    }

    pub fn last_config(&self) -> Option<&IrisConfig> {
        self.last_cfg.as_ref()
    }
}
