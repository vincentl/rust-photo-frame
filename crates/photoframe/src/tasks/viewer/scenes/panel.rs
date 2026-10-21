//! A CPU-rasterized panel composited onto the frame: text (cosmic-text
//! shaping, swash rasterization) and anything else drawn into one RGBA
//! buffer, uploaded as one plain texture, and drawn as one scissored
//! triangle. Shared by the showcase caption and the Wi-Fi overlay.
//!
//! Two V3D lessons are built in. glyphon's GPU glyph atlas is not used: its
//! eviction and growth corrupt glyphs on the Pi. And the panel is drawn as a
//! single oversized triangle clipped by the scissor, never as a quad
//! (`caption_composite.wgsl` explains the tile-binner bug that avoids).

use glyphon::{Buffer, Color as GlyphonColor, FontSystem, SwashCache};

/// Uniform for compositing the cached panel texture (must match
/// caption_composite.wgsl).
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct CompositeUniforms {
    resolution: [f32; 2],
    _pad0: [f32; 2],
    rect: [f32; 4],
}

/// The cached panel texture and the pipeline that composites it.
pub(super) struct PanelCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The panel texture's format: `Rgba8Unorm` holds linear premultiplied
    /// bytes (the caption), `Rgba8UnormSrgb` holds sRGB bytes, which keeps
    /// an opaque panel's colours exact (the overlay must match the clear
    /// colour around it to the bit).
    texel_format: wgpu::TextureFormat,
    pub(super) texture: Option<wgpu::Texture>,
    pub(super) dims: (u32, u32),
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
}

impl PanelCompositor {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        texel_format: wgpu::TextureFormat,
        label: &str,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(include_str!("caption_composite.wgsl").into()),
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(label),
            // 1:1 blit (the panel is rendered at its exact pixel size), so
            // text and QR modules stay crisp.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<CompositeUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
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
                    format: target_format,
                    // The panel holds premultiplied alpha: src + dst * (1 - src.a).
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
            device: device.clone(),
            queue: queue.clone(),
            texel_format,
            texture: None,
            dims: (0, 0),
            pipeline,
            layout,
            sampler,
            uniform_buffer,
            bind_group: None,
        }
    }

    /// Process ready GPU callbacks without blocking.
    pub(super) fn poll(&self) -> Result<wgpu::PollStatus, wgpu::PollError> {
        self.device.poll(wgpu::PollType::Poll)
    }

    /// Upload a `w × h` RGBA panel (in `texel_format`'s encoding), replacing
    /// the cached one; the texture is reallocated only when the size changes.
    pub(super) fn upload(&mut self, w: u32, h: u32, rgba: &[u8]) {
        debug_assert_eq!(rgba.len(), (w * h * 4) as usize);
        if self.texture.is_none() || self.dims != (w, h) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panel-cache-texture"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.texel_format,
                // COPY_DST for the upload; COPY_SRC for tests.
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("panel-composite-bind-group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.texture = Some(texture);
            self.bind_group = Some(bind_group);
            self.dims = (w, h);
        }
        let texture = self.texture.as_ref().expect("panel texture created above");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Draw the cached panel with its top-left corner at `(x, y)` on a
    /// `surface` sized target. The scissor, not the geometry, bounds the
    /// panel; it is clamped to the surface (wgpu rejects a scissor that
    /// hangs past the attachment), so a panel wider than a small surface
    /// renders partially instead of panicking. False when nothing was drawn.
    pub(super) fn composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        surface: (u32, u32),
        x: u32,
        y: u32,
    ) -> bool {
        let Some(bind_group) = self.bind_group.as_ref() else {
            return false;
        };
        let (cw, ch) = self.dims;
        let scissor_w = cw.min(surface.0.saturating_sub(x));
        let scissor_h = ch.min(surface.1.saturating_sub(y));
        if scissor_w == 0 || scissor_h == 0 {
            return false;
        }
        let uniforms = CompositeUniforms {
            resolution: [surface.0 as f32, surface.1 as f32],
            _pad0: [0.0, 0.0],
            rect: [x as f32, y as f32, cw as f32, ch as f32],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("panel-composite"),
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
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_scissor_rect(x, y, scissor_w, scissor_h);
        pass.draw(0..3, 0..1);
        true
    }
}

/// How glyph coverage is blended into a panel buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Encoding {
    /// Bytes are linear, premultiplied (`Rgba8Unorm` panels).
    LinearPremultiplied,
    /// Bytes are sRGB and the panel is opaque (`Rgba8UnormSrgb` panels):
    /// coverage is blended in linear light and re-encoded per pixel.
    SrgbOpaque,
}

pub(super) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Draw a shaped `buffer`'s glyphs in `color` onto `dst` (`dst_w × dst_h`
/// RGBA) with the buffer's origin at `origin`, clipped to `clip`
/// (`x0, y0, x1, y1`, exclusive ends).
#[allow(clippy::too_many_arguments)]
pub(super) fn blend_glyphs(
    buffer: &Buffer,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    color: GlyphonColor,
    dst: &mut [u8],
    dst_w: u32,
    origin: (i32, i32),
    clip: (i32, i32, i32, i32),
    encoding: Encoding,
) {
    let (x0, y0, x1, y1) = clip;
    buffer.draw(font_system, swash_cache, color, |gx, gy, gw, gh, color| {
        let a = color.a() as f32 / 255.0;
        if a <= 0.0 {
            return;
        }
        let sr = srgb_to_linear(color.r() as f32 / 255.0);
        let sg = srgb_to_linear(color.g() as f32 / 255.0);
        let sb = srgb_to_linear(color.b() as f32 / 255.0);
        let inv = 1.0 - a;
        for row in 0..gh as i32 {
            let py = origin.1 + gy + row;
            if py < y0 || py >= y1 {
                continue;
            }
            for col in 0..gw as i32 {
                let px = origin.0 + gx + col;
                if px < x0 || px >= x1 {
                    continue;
                }
                let idx = ((py as u32 * dst_w + px as u32) * 4) as usize;
                let to_byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
                match encoding {
                    Encoding::LinearPremultiplied => {
                        let d = |i: usize| dst[idx + i] as f32 / 255.0;
                        let (dr, dg, db, da) = (d(0), d(1), d(2), d(3));
                        dst[idx] = to_byte(sr * a + dr * inv);
                        dst[idx + 1] = to_byte(sg * a + dg * inv);
                        dst[idx + 2] = to_byte(sb * a + db * inv);
                        dst[idx + 3] = to_byte(a + da * inv);
                    }
                    Encoding::SrgbOpaque => {
                        let d = |i: usize| srgb_to_linear(dst[idx + i] as f32 / 255.0);
                        let (dr, dg, db) = (d(0), d(1), d(2));
                        dst[idx] = to_byte(linear_to_srgb(sr * a + dr * inv));
                        dst[idx + 1] = to_byte(linear_to_srgb(sg * a + dg * inv));
                        dst[idx + 2] = to_byte(linear_to_srgb(sb * a + db * inv));
                        dst[idx + 3] = 255;
                    }
                }
            }
        }
    });
}
