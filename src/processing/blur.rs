use std::sync::{mpsc, Mutex, OnceLock};

use anyhow::{anyhow, Context};
use bytemuck::{bytes_of, Pod, Zeroable};
use image::RgbaImage;
use tracing::{debug, warn};
use wgpu::util::DeviceExt;

const MAX_RADIUS: u32 = 24;

static GPU_BLUR: OnceLock<Mutex<Option<BlurContext>>> = OnceLock::new();

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurUniforms {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

/// Attempts to blur the provided RGBA image using a compute shader. If the GPU
/// is unavailable or any wgpu operation fails, `None` is returned so the caller
/// can gracefully fall back to a CPU-based blur.
pub fn try_gpu_blur(image: &RgbaImage, sigma: f32) -> Option<RgbaImage> {
    let radius = match sigma_to_radius(sigma) {
        Some(r) => r,
        None => return None,
    };

    let lock = GPU_BLUR.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().ok()?;
    if guard.is_none() {
        match BlurContext::new() {
            Ok(ctx) => {
                debug!("gpu blur context initialised");
                *guard = Some(ctx);
            }
            Err(err) => {
                warn!("failed to initialise gpu blur context: {err}");
                return None;
            }
        }
    }

    let blur_result = match guard.as_ref() {
        Some(ctx) => ctx.blur(image, radius),
        None => return None,
    };
    match blur_result {
        Ok(img) => Some(img),
        Err(err) => {
            warn!("gpu blur failed, falling back to cpu: {err}");
            // Drop the context so that subsequent calls attempt to recreate it.
            *guard = None;
            None
        }
    }
}

fn sigma_to_radius(sigma: f32) -> Option<u32> {
    if !sigma.is_finite() {
        return None;
    }
    let sigma = sigma.max(0.0);
    if sigma < 0.5 {
        return None;
    }
    let radius = ((sigma / 2.0).ceil() as u32).max(1);
    Some(radius.min(MAX_RADIUS))
}

struct BlurContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bind_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl BlurContext {
    fn new() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|err| anyhow!("failed to request adapter: {err}"))?;

        let format_features = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm);
        if !format_features
            .allowed_usages
            .contains(wgpu::TextureUsages::STORAGE_BINDING)
        {
            return Err(anyhow!("adapter does not support rgba8 storage textures"));
        }

        let required_features = wgpu::Features::empty();
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gpu-blur-device"),
            required_features,
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        }))
        .map_err(|err| anyhow!("failed to request device: {err}"))?;

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gpu-blur-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gpu-blur-pipeline-layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gpu-blur-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("./shaders/box_blur.comp.wgsl").into()),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("gpu-blur-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Ok(Self {
            device,
            queue,
            bind_layout,
            pipeline,
        })
    }

    fn blur(&self, image: &RgbaImage, radius: u32) -> anyhow::Result<RgbaImage> {
        let width = image.width();
        let height = image.height();
        if width == 0 || height == 0 {
            return Err(anyhow!("image has zero dimensions"));
        }
        let texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let input_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-src-texture"),
            size: texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let input_view = input_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let output_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-dst-texture"),
            size: texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let uniforms = BlurUniforms {
            width,
            height,
            radius,
            _pad: 0,
        };
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur-params"),
                contents: bytes_of(&uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu-blur-bind-group"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            image.as_raw(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            texture_size,
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu-blur-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("gpu-blur-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let wg_x = divide_rounding_up(width, 8);
            let wg_y = divide_rounding_up(height, 8);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }

        let bytes_per_row = 4 * width;
        let padded_bytes_per_row = align_to(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let output_buffer_size = padded_bytes_per_row as u64 * height as u64;
        let staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur-readback"),
            size: output_buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            texture_size,
        );

        let slice = staging_buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = sender.send(res);
        });
        self.queue.submit(Some(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::Wait);
        match receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(anyhow!("map_async failed: {err}")),
            Err(_) => return Err(anyhow!("map_async callback dropped")),
        }

        let data = slice.get_mapped_range();
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let row_bytes = (width * 4) as usize;
        let padded_row_bytes = padded_bytes_per_row as usize;
        if row_bytes == padded_row_bytes {
            pixels.copy_from_slice(&data);
        } else {
            for (dst_row, src_row) in pixels
                .chunks_mut(row_bytes)
                .zip(data.chunks(padded_row_bytes))
            {
                dst_row.copy_from_slice(&src_row[..row_bytes]);
            }
        }
        drop(data);
        staging_buffer.unmap();

        RgbaImage::from_raw(width, height, pixels).context("gpu blur produced invalid buffer")
    }
}

fn divide_rounding_up(value: u32, divisor: u32) -> u32 {
    (value + divisor - 1) / divisor
}

fn align_to(value: u32, alignment: u32) -> u32 {
    if alignment == 0 {
        return value;
    }
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value + (alignment - remainder)
    }
}
