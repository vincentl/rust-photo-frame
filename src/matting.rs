use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use image::{imageops, DynamicImage, RgbaImage};
use wgpu::util::DeviceExt;

use crate::config::BlurBackend;

const MAX_KERNEL_RADIUS: u32 = 32;
const MAX_TAPS: usize = (MAX_KERNEL_RADIUS as usize) * 2 + 1;
const KERNEL_SIZE: usize = ((MAX_TAPS + 3) / 4) * 4;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurUniforms {
    width: u32,
    height: u32,
    radius: u32,
    direction: u32,
    weights: [f32; KERNEL_SIZE],
}

struct ComputeContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_layout: wgpu::BindGroupLayout,
}

impl ComputeContext {
    fn new() -> Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .context("failed to acquire compute adapter")?;
        let limits = adapter.limits();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("blur-compute-device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            }))?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gaussian-blur"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "shaders/matting_blur.wgsl"
            ))),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur-bind-layout"),
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
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<BlurUniforms>() as u64,
                        ),
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
            bind_layout,
        })
    }

    fn blur(&self, image: &RgbaImage, sigma: f32) -> Result<RgbaImage> {
        let width = image.width();
        let height = image.height();
        if width == 0 || height == 0 {
            return Err(anyhow!("image has zero dimension"));
        }
        let radius = compute_radius(sigma);
        if radius == 0 {
            return Ok(image.clone());
        }
        let weights = build_weights(radius as usize, sigma);
        let uniforms = BlurUniforms {
            width,
            height,
            radius,
            direction: 0,
            weights,
        };
        let temp_uniforms = BlurUniforms {
            direction: 1,
            ..uniforms
        };
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let src_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-src"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let tmp_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-tmp"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let dst_tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-dst"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let stride = width * 4;
        let padded = compute_padded_stride(stride);
        let mut staging = Vec::with_capacity((padded * height) as usize);
        if padded != stride {
            staging.resize((padded * height) as usize, 0);
            for y in 0..height as usize {
                let src_offset = y * (stride as usize);
                let dst_offset = y * (padded as usize);
                staging[dst_offset..dst_offset + stride as usize]
                    .copy_from_slice(&image.as_raw()[src_offset..src_offset + stride as usize]);
            }
        }
        let bytes = if padded == stride {
            Cow::Borrowed(image.as_raw())
        } else {
            Cow::Owned(staging)
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
            extent,
        );
        let src_view = src_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let tmp_view = tmp_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let uniforms_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur-uniforms-0"),
                contents: bytemuck::bytes_of(&uniforms),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let uniforms_buf_v = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur-uniforms-1"),
                contents: bytemuck::bytes_of(&temp_uniforms),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let bind_horizontal = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-horizontal"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&tmp_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniforms_buf.as_entire_binding(),
                },
            ],
        });
        let bind_vertical = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-vertical"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&tmp_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&dst_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniforms_buf_v.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blur-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("blur-pass-horizontal"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_horizontal, &[]);
            let x_groups = (width + 7) / 8;
            let y_groups = (height + 7) / 8;
            pass.dispatch_workgroups(x_groups, y_groups, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("blur-pass-vertical"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_vertical, &[]);
            let x_groups = (width + 7) / 8;
            let y_groups = (height + 7) / 8;
            pass.dispatch_workgroups(x_groups, y_groups, 1);
        }
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur-readback"),
            size: (padded as u64) * (height as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &dst_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        let _ = self.device.poll(wgpu::PollType::Wait);
        let buffer_slice = output_buffer.slice(..);
        buffer_slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::Wait);
        let data = buffer_slice.get_mapped_range();
        let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
        if padded == stride {
            let count = out.len();
            out.copy_from_slice(&data[..count]);
        } else {
            for y in 0..height as usize {
                let src_offset = y * (padded as usize);
                let dst_offset = y * (stride as usize);
                out[dst_offset..dst_offset + stride as usize]
                    .copy_from_slice(&data[src_offset..src_offset + stride as usize]);
            }
        }
        drop(data);
        output_buffer.unmap();
        Ok(RgbaImage::from_raw(width, height, out).expect("valid rgba dimensions"))
    }
}

fn compute_padded_stride(bytes_per_row: u32) -> u32 {
    const ALIGN: u32 = 256;
    if bytes_per_row == 0 {
        return 0;
    }
    bytes_per_row.div_ceil(ALIGN) * ALIGN
}

fn compute_radius(sigma: f32) -> u32 {
    if sigma <= 0.0 {
        return 0;
    }
    let sigma = sigma.max(0.1);
    let radius = (sigma * 3.0).ceil() as u32;
    radius.min(MAX_KERNEL_RADIUS)
}

fn build_weights(radius: usize, sigma: f32) -> [f32; KERNEL_SIZE] {
    let mut weights = [0.0f32; KERNEL_SIZE];
    if radius == 0 {
        weights[0] = 1.0;
        return weights;
    }
    let sigma_sq = sigma * sigma;
    let mut sum = 0.0f32;
    let taps = radius * 2 + 1;
    for i in 0..taps {
        let offset = i as i32 - radius as i32;
        let weight = (-((offset * offset) as f32) / (2.0 * sigma_sq)).exp();
        weights[i] = weight;
        sum += weight;
    }
    if sum > 0.0 {
        for w in &mut weights[..taps] {
            *w /= sum;
        }
    } else {
        weights[0] = 1.0;
    }
    weights
}

static COMPUTE_CONTEXT: OnceLock<Option<Arc<ComputeContext>>> = OnceLock::new();

fn compute_context() -> Option<Arc<ComputeContext>> {
    COMPUTE_CONTEXT
        .get_or_init(|| ComputeContext::new().map(Arc::new).ok())
        .clone()
}

pub fn blur_image(image: RgbaImage, sigma: f32, backend: BlurBackend) -> RgbaImage {
    if sigma <= 0.0 {
        return image;
    }
    match backend {
        BlurBackend::Cpu => blur_cpu(&image, sigma),
        BlurBackend::Wgpu => compute_context()
            .and_then(|ctx| ctx.blur(&image, sigma).ok())
            .unwrap_or_else(|| blur_cpu(&image, sigma)),
        BlurBackend::Neon => blur_neon(&image, sigma).unwrap_or_else(|| blur_cpu(&image, sigma)),
    }
}

fn blur_cpu(image: &RgbaImage, sigma: f32) -> RgbaImage {
    let dynamic = DynamicImage::ImageRgba8(image.clone());
    imageops::blur(&dynamic, sigma)
}

#[cfg(target_arch = "aarch64")]
fn blur_neon(image: &RgbaImage, sigma: f32) -> Option<RgbaImage> {
    use std::arch::aarch64::*;

    if image.width() == 0 || image.height() == 0 {
        return None;
    }
    let passes = ((sigma / 1.5).round().clamp(1.0, 6.0)) as usize;
    let mut current = image.clone();
    let width = image.width() as usize;
    let height = image.height() as usize;
    let stride = width * 4;
    let mut buffer = vec![0u16; stride * height];
    let mut vertical = vec![0u16; stride * height];

    for _ in 0..passes {
        for y in 0..height {
            let row = &current.as_raw()[y * stride..(y + 1) * stride];
            let dst = &mut buffer[y * stride..(y + 1) * stride];
            unsafe {
                horizontal_box_blur(row, dst);
            }
        }

        for y in 0..height {
            let top = if y == 0 { y } else { y - 1 };
            let mid = y;
            let bot = if y + 1 >= height { y } else { y + 1 };
            unsafe {
                vertical_box_blur(
                    &buffer[top * stride..(top + 1) * stride],
                    &buffer[mid * stride..(mid + 1) * stride],
                    &buffer[bot * stride..(bot + 1) * stride],
                    &mut vertical[y * stride..(y + 1) * stride],
                );
            }
        }

        let mut out = vec![0u8; current.as_raw().len()];
        for (chunk, dst) in vertical
            .chunks_exact(stride)
            .zip(out.chunks_exact_mut(stride))
        {
            for (val, byte) in chunk.iter().zip(dst.iter_mut()) {
                *byte = (*val).clamp(0, 255) as u8;
            }
        }
        current = RgbaImage::from_raw(image.width(), image.height(), out)?;
    }

    Some(current)
}

#[cfg(target_arch = "aarch64")]
unsafe fn horizontal_box_blur(src: &[u8], dst: &mut [u16]) {
    let len = src.len().min(dst.len());
    if len == 0 {
        return;
    }
    if len < 16 {
        scalar_horizontal_box_blur(src, dst);
        return;
    }

    let mut prev = vld1q_u8(src.as_ptr());
    let mut offset = 0usize;
    while offset + 16 <= len {
        let curr = if offset == 0 {
            prev
        } else {
            vld1q_u8(src.as_ptr().add(offset))
        };
        let next = if offset + 16 < len {
            vld1q_u8(src.as_ptr().add(offset + 16))
        } else {
            curr
        };
        let left = if offset == 0 {
            curr
        } else {
            vextq_u8(prev, curr, 12)
        };
        let right = if offset + 16 >= len {
            curr
        } else {
            vextq_u8(curr, next, 4)
        };
        let left_lo = vget_low_u8(left);
        let left_hi = vget_high_u8(left);
        let center_lo = vget_low_u8(curr);
        let center_hi = vget_high_u8(curr);
        let right_lo = vget_low_u8(right);
        let right_hi = vget_high_u8(right);
        let sum_lo = vaddw_u8(vaddl_u8(left_lo, center_lo), right_lo);
        let sum_hi = vaddw_u8(vaddl_u8(left_hi, center_hi), right_hi);
        vst1q_u16(dst.as_mut_ptr().add(offset), sum_lo);
        vst1q_u16(dst.as_mut_ptr().add(offset + 8), sum_hi);
        prev = curr;
        offset += 16;
    }

    if offset < len {
        scalar_horizontal_box_blur(&src[offset..], &mut dst[offset..]);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn vertical_box_blur(top: &[u16], mid: &[u16], bot: &[u16], out: &mut [u16]) {
    let len = top.len().min(mid.len()).min(bot.len()).min(out.len());
    let mut offset = 0usize;
    while offset + 8 <= len {
        let t = vld1q_u16(top.as_ptr().add(offset));
        let m = vld1q_u16(mid.as_ptr().add(offset));
        let b = vld1q_u16(bot.as_ptr().add(offset));
        let sum = vaddq_u16(vaddq_u16(t, m), b);
        let sum_lo = vmovl_u16(vget_low_u16(sum));
        let sum_hi = vmovl_u16(vget_high_u16(sum));
        let scale = vdupq_n_f32(1.0 / 9.0);
        let f_lo = vmulq_f32(vcvtq_f32_u32(sum_lo), scale);
        let f_hi = vmulq_f32(vcvtq_f32_u32(sum_hi), scale);
        let rounded_lo = vcvtnq_u32_f32(f_lo);
        let rounded_hi = vcvtnq_u32_f32(f_hi);
        let packed = vcombine_u16(vqmovn_u32(rounded_lo), vqmovn_u32(rounded_hi));
        vst1q_u16(out.as_mut_ptr().add(offset), packed);
        offset += 8;
    }

    for idx in offset..len {
        let sum = top[idx] as u32 + mid[idx] as u32 + bot[idx] as u32;
        out[idx] = ((sum as f32) * (1.0 / 9.0)).round() as u16;
    }
}

#[cfg(target_arch = "aarch64")]
fn scalar_horizontal_box_blur(src: &[u8], dst: &mut [u16]) {
    let len = src.len().min(dst.len());
    if len == 0 {
        return;
    }
    for idx in 0..len {
        let left_idx = if idx >= 4 { idx - 4 } else { idx };
        let right_idx = if idx + 4 < len { idx + 4 } else { idx };
        let l = src[left_idx] as u16;
        let m = src[idx] as u16;
        let r = src[right_idx] as u16;
        dst[idx] = l + m + r;
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn blur_neon(_image: &RgbaImage, _sigma: f32) -> Option<RgbaImage> {
    None
}
