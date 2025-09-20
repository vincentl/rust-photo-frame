use std::{borrow::Cow, num::NonZeroU64};

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct BlurParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

struct TextureBundle {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl TextureBundle {
    fn new(device: &wgpu::Device, desc: &wgpu::TextureDescriptor) -> Self {
        let texture = device.create_texture(desc);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BlurSettings {
    pub width: u32,
    pub height: u32,
    pub radius: u32,
    pub sigma: f32,
}

pub struct BlurContext {
    format: wgpu::TextureFormat,
    params_buffer: wgpu::Buffer,
    weights_buffer: wgpu::Buffer,
    weights_capacity: usize,
    bind_layout: wgpu::BindGroupLayout,
    horizontal_pipeline: wgpu::ComputePipeline,
    vertical_pipeline: wgpu::ComputePipeline,
    intermediates: [Option<TextureBundle>; 2],
    current_size: Option<(u32, u32)>,
}

impl BlurContext {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur-params"),
            size: std::mem::size_of::<BlurParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let weights_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur-weights"),
            size: 4, // allocate minimal buffer, will grow on demand
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: Some(
                            NonZeroU64::new(std::mem::size_of::<BlurParams>() as u64).unwrap(),
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur-pipeline-layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });

        let horizontal_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur-horizontal-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "shaders/gaussian_horizontal.wgsl"
            ))),
        });

        let vertical_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur-vertical-shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "shaders/gaussian_vertical.wgsl"
            ))),
        });

        let horizontal_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("blur-horizontal"),
                layout: Some(&pipeline_layout),
                module: &horizontal_shader,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });

        let vertical_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blur-vertical"),
            layout: Some(&pipeline_layout),
            module: &vertical_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Self {
            format,
            params_buffer,
            weights_buffer,
            weights_capacity: 1,
            bind_layout,
            horizontal_pipeline,
            vertical_pipeline,
            intermediates: [None, None],
            current_size: None,
        }
    }

    fn ensure_intermediates(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if self.current_size == Some((width, height)) {
            return;
        }
        if width == 0 || height == 0 {
            self.intermediates = [None, None];
            self.current_size = None;
            return;
        }
        let desc = wgpu::TextureDescriptor {
            label: Some("blur-intermediate"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        };
        self.intermediates = [
            Some(TextureBundle::new(device, &desc)),
            Some(TextureBundle::new(device, &desc)),
        ];
        self.current_size = Some((width, height));
    }

    fn ensure_weights(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, weights: &[f32]) {
        if weights.is_empty() {
            return;
        }
        if weights.len() > self.weights_capacity {
            self.weights_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blur-weights"),
                size: std::mem::size_of_val(weights) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.weights_capacity = weights.len();
        }
        queue.write_buffer(&self.weights_buffer, 0, bytemuck::cast_slice(weights));
    }

    pub fn blur_to_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        destination: &wgpu::Texture,
        settings: BlurSettings,
    ) {
        if settings.width == 0 || settings.height == 0 {
            return;
        }

        self.ensure_intermediates(device, settings.width, settings.height);
        let weights = compute_weights(settings.radius, settings.sigma);
        self.ensure_weights(device, queue, &weights);

        let params = BlurParams {
            width: settings.width,
            height: settings.height,
            radius: settings.radius,
            _pad: 0,
        };
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(&params));

        let Some(intermediate_a) = self.intermediates[0].as_ref() else {
            return;
        };
        let Some(intermediate_b) = self.intermediates[1].as_ref() else {
            return;
        };

        let horizontal_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-horizontal-bind"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&intermediate_a.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.weights_buffer.as_entire_binding(),
                },
            ],
        });

        let vertical_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-vertical-bind"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&intermediate_a.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&intermediate_b.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.weights_buffer.as_entire_binding(),
                },
            ],
        });

        let dispatch_x = settings.width.div_ceil(16);
        let dispatch_y = settings.height.div_ceil(16);

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("blur-horizontal-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.horizontal_pipeline);
            pass.set_bind_group(0, &horizontal_bind, &[]);
            pass.dispatch_workgroups(dispatch_x, dispatch_y, 1);
        }

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("blur-vertical-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.vertical_pipeline);
            pass.set_bind_group(0, &vertical_bind, &[]);
            pass.dispatch_workgroups(dispatch_x, dispatch_y, 1);
        }

        let extent = wgpu::Extent3d {
            width: settings.width,
            height: settings.height,
            depth_or_array_layers: 1,
        };

        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &intermediate_b.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: destination,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            extent,
        );
    }
}

fn compute_weights(radius: u32, sigma: f32) -> Vec<f32> {
    let taps = radius.saturating_mul(2).saturating_add(1) as usize;
    if taps == 0 {
        return vec![1.0];
    }
    let effective_sigma = if sigma > 0.0 {
        sigma
    } else {
        (radius.max(1) as f32) / 2.0
    };
    let denom = 2.0 * effective_sigma * effective_sigma;
    let mut weights = Vec::with_capacity(taps);
    let radius_i = radius as i32;
    for offset in -radius_i..=radius_i {
        let dist = offset as f32;
        let weight = (-dist * dist / denom).exp();
        weights.push(weight);
    }
    let sum: f32 = weights.iter().copied().sum();
    if sum > 0.0 {
        for w in &mut weights {
            *w /= sum;
        }
    } else {
        let uniform = 1.0 / (taps as f32);
        weights.fill(uniform);
    }
    weights
}
