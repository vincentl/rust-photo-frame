use bytemuck::{Pod, Zeroable};
use image::{imageops, RgbaImage};
use tracing::warn;

use crate::config::BlurBackend;

pub fn apply_blur(image: &RgbaImage, sigma: f32, backend: BlurBackend) -> RgbaImage {
    if sigma <= 0.0 {
        return image.clone();
    }
    match backend {
        BlurBackend::Cpu => blur_cpu(image, sigma),
        BlurBackend::Neon => neon_blur(image, sigma).unwrap_or_else(|| blur_cpu(image, sigma)),
        BlurBackend::WgpuCompute => {
            gpu_blur(image, sigma).unwrap_or_else(|| blur_cpu(image, sigma))
        }
        BlurBackend::Auto => {
            #[cfg(target_arch = "aarch64")]
            {
                if let Some(img) = neon_blur(image, sigma) {
                    return img;
                }
            }
            if let Some(img) = gpu_blur(image, sigma) {
                return img;
            }
            blur_cpu(image, sigma)
        }
    }
}

fn blur_cpu(image: &RgbaImage, sigma: f32) -> RgbaImage {
    imageops::blur(image, sigma)
}

fn gaussian_kernel(sigma: f32) -> (Vec<f32>, u32) {
    let sigma = sigma.max(0.01);
    let radius = (sigma * 3.0).ceil() as i32;
    if radius <= 0 {
        return (vec![1.0], 0);
    }
    let mut weights = Vec::with_capacity((radius * 2 + 1) as usize);
    let denom = 2.0 * sigma * sigma;
    let mut sum = 0.0;
    for i in -radius..=radius {
        let x = i as f32;
        let w = (-x * x / denom).exp();
        weights.push(w);
        sum += w;
    }
    if sum > 0.0 {
        for w in &mut weights {
            *w /= sum;
        }
    }
    (weights, radius as u32)
}

fn rgba_to_f32(image: &RgbaImage) -> Vec<f32> {
    image
        .pixels()
        .flat_map(|p| p.0.iter().map(|&c| (c as f32) / 255.0))
        .collect()
}

fn f32_to_rgba(width: u32, height: u32, data: &[f32]) -> RgbaImage {
    let mut out = RgbaImage::new(width, height);
    for (i, pixel) in out.pixels_mut().enumerate() {
        let base = i * 4;
        let rgba = [
            (data.get(base).copied().unwrap_or(0.0) * 255.0).clamp(0.0, 255.0) as u8,
            (data.get(base + 1).copied().unwrap_or(0.0) * 255.0).clamp(0.0, 255.0) as u8,
            (data.get(base + 2).copied().unwrap_or(0.0) * 255.0).clamp(0.0, 255.0) as u8,
            (data.get(base + 3).copied().unwrap_or(1.0) * 255.0).clamp(0.0, 255.0) as u8,
        ];
        pixel.0 = rgba;
    }
    out
}

#[cfg(target_arch = "aarch64")]
fn neon_blur(image: &RgbaImage, sigma: f32) -> Option<RgbaImage> {
    if !std::arch::is_aarch64_feature_detected!("neon") {
        return None;
    }
    let (weights, radius) = gaussian_kernel(sigma);
    if radius == 0 {
        return Some(image.clone());
    }
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut src = rgba_to_f32(image);
    let mut tmp = vec![0.0f32; src.len()];
    unsafe {
        neon::blur_pass(
            &src,
            &mut tmp,
            width,
            height,
            radius as usize,
            &weights,
            true,
        );
        neon::blur_pass(
            &tmp,
            &mut src,
            width,
            height,
            radius as usize,
            &weights,
            false,
        );
    }
    Some(f32_to_rgba(image.width(), image.height(), &src))
}

#[cfg(not(target_arch = "aarch64"))]
fn neon_blur(_image: &RgbaImage, _sigma: f32) -> Option<RgbaImage> {
    None
}

fn gpu_blur(image: &RgbaImage, sigma: f32) -> Option<RgbaImage> {
    let (weights, radius) = gaussian_kernel(sigma);
    if radius == 0 {
        return Some(image.clone());
    }
    let width = image.width();
    let height = image.height();
    let data = rgba_to_f32(image);
    let ctx = match gpu::instance() {
        Some(ctx) => ctx,
        None => return None,
    };
    let mut guard = ctx.lock().expect("gpu blur mutex poisoned");
    let result = guard.run(width, height, radius, &weights, &data);
    drop(guard);
    match result {
        Ok(result) => Some(f32_to_rgba(width, height, &result)),
        Err(err) => {
            warn!("wgpu compute blur fallback: {err:?}");
            None
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    pub unsafe fn blur_pass(
        src: &[f32],
        dst: &mut [f32],
        width: usize,
        height: usize,
        radius: usize,
        weights: &[f32],
        horizontal: bool,
    ) {
        let src_ptr = src.as_ptr();
        let dst_ptr = dst.as_mut_ptr();
        let kernel = &weights[..(2 * radius + 1)];
        for y in 0..height {
            for x in 0..width {
                let mut acc = vdupq_n_f32(0.0);
                for (idx, &weight) in kernel.iter().enumerate() {
                    let offset = idx as isize - radius as isize;
                    let sample_index = if horizontal {
                        let sx = clamp_i((x as isize) + offset, width as isize);
                        ((y * width) + sx) * 4
                    } else {
                        let sy = clamp_i((y as isize) + offset, height as isize);
                        ((sy * width) + x) * 4
                    } as isize;
                    let pix = vld1q_f32(src_ptr.offset(sample_index));
                    let weight_vec = vdupq_n_f32(weight);
                    acc = vmlaq_f32(acc, pix, weight_vec);
                }
                let out_index = ((y * width + x) * 4) as isize;
                vst1q_f32(dst_ptr.offset(out_index), acc);
            }
        }
    }

    #[inline(always)]
    fn clamp_i(value: isize, max: isize) -> usize {
        value.clamp(0, max.saturating_sub(1)) as usize
    }
}

mod gpu {
    use super::BlurUniform;
    use anyhow::{anyhow, Context, Result};
    use bytemuck::cast_slice;
    use std::sync::{mpsc, Arc, Mutex, OnceLock};

    use wgpu::util::DeviceExt;

    static CONTEXT: OnceLock<Result<Arc<Mutex<GpuBlurContext>>>> = OnceLock::new();

    pub fn instance() -> Option<Arc<Mutex<GpuBlurContext>>> {
        match CONTEXT.get_or_init(|| GpuBlurContext::new().map(Mutex::new).map(Arc::new)) {
            Ok(ctx) => Some(Arc::clone(ctx)),
            Err(err) => {
                tracing::warn!("failed to init gpu blur context: {err:?}");
                None
            }
        }
    }

    pub struct GpuBlurContext {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipeline: wgpu::ComputePipeline,
        layout: wgpu::BindGroupLayout,
    }

    impl GpuBlurContext {
        fn new() -> Result<Self> {
            let instance = wgpu::Instance::default();
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                }))
                .context("request wgpu adapter")?;
            let limits = adapter.limits();
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("blur-compute-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::default(),
                }))
                .context("request wgpu device")?;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("gaussian-blur-compute"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../tasks/shaders/gaussian_blur.comp.wgsl").into(),
                ),
            });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("gaussian-blur-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
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
                label: Some("gaussian-blur-pipeline-layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("gaussian-blur-pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
            Ok(Self {
                device,
                queue,
                pipeline,
                layout,
            })
        }

        pub fn run(
            &mut self,
            width: u32,
            height: u32,
            radius: u32,
            weights: &[f32],
            input: &[f32],
        ) -> Result<Vec<f32>> {
            let pixel_count = (width as usize) * (height as usize);
            if input.len() != pixel_count * 4 {
                return Err(anyhow!("unexpected input length"));
            }
            let buffer_size = (input.len() * std::mem::size_of::<f32>()) as u64;
            let src = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("blur-src"),
                    contents: cast_slice(input),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                });
            let tmp = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blur-tmp"),
                size: buffer_size as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let dst = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blur-dst"),
                size: buffer_size as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let weights_buf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("blur-weights"),
                    contents: cast_slice(weights),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                });
            let stages = [(0u32, &src, &tmp), (1u32, &tmp, &dst)];
            for (direction, src_buf, dst_buf) in stages.iter() {
                let uniforms = BlurUniform {
                    width,
                    height,
                    radius,
                    direction: *direction,
                };
                let uniform_buf =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("blur-uniform"),
                            contents: cast_slice(&[uniforms]),
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_SRC,
                        });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("blur-bind"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(
                                uniform_buf.as_entire_buffer_binding(),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Buffer(
                                src_buf.as_entire_buffer_binding(),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Buffer(
                                dst_buf.as_entire_buffer_binding(),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Buffer(
                                weights_buf.as_entire_buffer_binding(),
                            ),
                        },
                    ],
                });
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("blur-command"),
                        });
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("blur-pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &bind, &[]);
                    let groups_x = (width + 15) / 16;
                    let groups_y = (height + 15) / 16;
                    pass.dispatch_workgroups(groups_x, groups_y, 1);
                }
                self.queue.submit(Some(encoder.finish()));
            }
            let buffer_slice = dst.slice(..);
            let (sender, receiver) = mpsc::channel();
            buffer_slice.map_async(wgpu::MapMode::Read, move |res| {
                let _ = sender.send(res);
            });
            let _ = self.device.poll(wgpu::PollType::Wait);
            receiver
                .recv()
                .context("receive map result")?
                .context("map buffer for read")?;
            let data = buffer_slice.get_mapped_range();
            let out = bytemuck::cast_slice(&data).to_vec();
            drop(data);
            dst.unmap();
            Ok(out)
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurUniform {
    width: u32,
    height: u32,
    radius: u32,
    direction: u32,
}
