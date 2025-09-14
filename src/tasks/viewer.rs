use crate::events::{Displayed, MatMode, PhotoLoaded, PreparedImageCpu, SurfaceSize};
use anyhow::Result;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::debug;

const FADE_MS: u64 = 400; // crossfade duration
const DWELL_MS: u64 = 1800; // time to sit on a slide after fade

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: wgpu::TextureView,
    target_tex: wgpu::Texture,
    target_w: u32,
    target_h: u32,
    sampler: wgpu::Sampler,
    img_pipeline: wgpu::RenderPipeline,
    blur_pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    img_bind_layout: wgpu::BindGroupLayout,
    limits: wgpu::Limits,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    // screen size
    screen_w: f32,
    screen_h: f32,
    // dest rect in pixels: x, y, w, h
    dest_x: f32,
    dest_y: f32,
    dest_w: f32,
    dest_h: f32,
    // global alpha for current draw
    alpha: f32,
    _pad: [f32; 3],
}

struct UploadedImage {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
    mat: MatMode,
    path: std::path::PathBuf,
}

pub async fn run(
    mut from_loader: Receiver<PhotoLoaded>,
    to_manager_displayed: Sender<Displayed>,
    mut size_rx: Receiver<SurfaceSize>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut gpu = init_gpu(1920, 1080).await?; // placeholder until first size arrives

    let mut current: Option<UploadedImage> = None;

    loop {
        select! {
            _ = cancel.cancelled() => break,
            Some(PhotoLoaded(prep)) = from_loader.recv() => {
                debug!("displaying: {}", prep.path.display());
                let next = upload_image(&gpu, &prep)?;

                if let Some(cur) = current.take() {
                    crossfade(&gpu, &cur, &next, Duration::from_millis(FADE_MS)).await?;
                    // dwell
                    sleep(Duration::from_millis(DWELL_MS)).await;
                    let _ = to_manager_displayed.send(Displayed(next.path.clone())).await;
                    current = Some(next);
                } else {
                    // First image: no crossfade, render once then dwell
                    render_modes(&gpu, &next, 1.0)?;
                    sleep(Duration::from_millis(DWELL_MS)).await;
                    let _ = to_manager_displayed.send(Displayed(next.path.clone())).await;
                    current = Some(next);
                }
            }
            ,
            Some(SurfaceSize { width, height, oversample }) = size_rx.recv() => {
                // Recreate target for new size derived from surface * oversample, clamped to device limits
                let desired_w = ((width as f32) * oversample).round().max(1.0) as u32;
                let desired_h = ((height as f32) * oversample).round().max(1.0) as u32;
                let w = desired_w.min(gpu.limits.max_texture_dimension_2d);
                let h = desired_h.min(gpu.limits.max_texture_dimension_2d);
                if w != gpu.target_w || h != gpu.target_h {
                    debug!(old_w = gpu.target_w, old_h = gpu.target_h, desired_w, desired_h, new_w = w, new_h = h, "viewer resize: render target size change (desired -> new; clamped if reduced)");
                    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("offscreen"),
                        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    });
                    gpu.target = tex.create_view(&wgpu::TextureViewDescriptor::default());
                    gpu.target_tex = tex;
                    gpu.target_w = w;
                    gpu.target_h = h;
                }
            }
        }
    }
    Ok(())
}

async fn init_gpu(init_w: u32, init_h: u32) -> Result<Gpu> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .ok_or_else(|| anyhow::anyhow!("no suitable GPU adapter found"))?;
    let limits = adapter.limits();
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("viewer-device"),
                required_features: wgpu::Features::empty(),
                required_limits: limits.clone(),
            },
            None,
        )
        .await?;

    // Offscreen target (clamp to device limits)
    let target_w = init_w.min(limits.max_texture_dimension_2d);
    let target_h = init_h.min(limits.max_texture_dimension_2d);
    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size: wgpu::Extent3d {
            width: target_w,
            height: target_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("linear-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // Uniforms: screen, dest_rect, alpha
    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("uniform-layout"),
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
    let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("uniform-bind"),
        layout: &uniform_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });

    // Image bind group layout (texture + sampler)
    let img_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("image-bind-layout"),
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

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("quad-shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
            "shaders/viewer_quad.wgsl"
        ))),
    });
    let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blur-shader"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
            "shaders/blur_bg.wgsl"
        ))),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&uniform_layout, &img_bind_layout],
        push_constant_ranges: &[],
    });

    let color_target = wgpu::ColorTargetState {
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    };

    let img_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("img-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            targets: &[Some(color_target.clone())],
        }),
        multiview: None,
    });

    let blur_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blur-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &blur_shader,
            entry_point: "vs_main",
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &blur_shader,
            entry_point: "fs_main",
            targets: &[Some(color_target)],
        }),
        multiview: None,
    });

    Ok(Gpu {
        device,
        queue,
        target,
        target_tex,
        target_w,
        target_h,
        sampler,
        img_pipeline,
        blur_pipeline,
        uniform_buf,
        uniform_bind,
        img_bind_layout,
        limits,
    })
}

fn upload_image(gpu: &Gpu, img: &PreparedImageCpu) -> Result<UploadedImage> {
    // Compute scaled size to fit within current target and device limit (no upscaling)
    let (out_w, out_h) = compute_scaled_size(
        img.width,
        img.height,
        gpu.target_w,
        gpu.target_h,
        1.0,
        gpu.limits.max_texture_dimension_2d,
    );
    let mut pixels: std::borrow::Cow<'_, [u8]> = std::borrow::Cow::Borrowed(&img.pixels);
    if out_w != img.width || out_h != img.height {
        debug!(
            src_w = img.width,
            src_h = img.height,
            dst_w = out_w,
            dst_h = out_h,
            "upload downscale to fit target/device limits"
        );
        if let Some(src) = image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone()) {
            let resized = image::imageops::resize(&src, out_w, out_h, image::imageops::FilterType::Triangle);
            pixels = std::borrow::Cow::Owned(resized.into_raw());
        }
    }

    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("photo-texture"),
        size: wgpu::Extent3d {
            width: out_w,
            height: out_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Ensure bytes_per_row meets wgpu's 256-byte alignment by padding if necessary.
    let row_stride = 4 * out_w;
    let padded_stride = compute_padded_stride(row_stride);
    let upload_bytes: std::borrow::Cow<'_, [u8]> = if padded_stride != row_stride {
        // Pad each row into a staging buffer
        let src = pixels.as_ref();
        let mut staging = vec![0u8; (padded_stride as usize) * (out_h as usize)];
        let row_src_len = row_stride as usize;
        let row_dst_len = padded_stride as usize;
        for y in 0..(out_h as usize) {
            let src_off = y * row_src_len;
            let dst_off = y * row_dst_len;
            staging[dst_off..dst_off + row_src_len]
                .copy_from_slice(&src[src_off..src_off + row_src_len]);
        }
        std::borrow::Cow::Owned(staging)
    } else {
        pixels
    };

    gpu.queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(padded_stride),
            rows_per_image: Some(out_h),
        },
        wgpu::Extent3d {
            width: out_w,
            height: out_h,
            depth_or_array_layers: 1,
        },
    );
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("image-bind"),
        layout: &gpu.img_bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
            },
        ],
    });

    Ok(UploadedImage {
        tex,
        view,
        bind,
        w: out_w,
        h: out_h,
        mat: img.mat.clone(),
        path: img.path.clone(),
    })
}

pub fn compute_padded_stride(bytes_per_row: u32) -> u32 {
    const ALIGN: u32 = 256;
    if bytes_per_row == 0 { return 0; }
    ((bytes_per_row + (ALIGN - 1)) / ALIGN) * ALIGN
}

pub fn compute_scaled_size(
    img_w: u32,
    img_h: u32,
    surf_w: u32,
    surf_h: u32,
    oversample: f32,
    max_dim: u32,
) -> (u32, u32) {
    if img_w == 0 || img_h == 0 || surf_w == 0 || surf_h == 0 { return (1,1); }
    let max_w = ((surf_w as f32) * oversample).round() as u32;
    let max_h = ((surf_h as f32) * oversample).round() as u32;
    let max_w = max_w.min(max_dim).max(1);
    let max_h = max_h.min(max_dim).max(1);
    let sw = (max_w as f32) / (img_w as f32);
    let sh = (max_h as f32) / (img_h as f32);
    let s = sw.min(sh).min(1.0);
    let out_w = ((img_w as f32) * s).floor().max(1.0) as u32;
    let out_h = ((img_h as f32) * s).floor().max(1.0) as u32;
    (out_w, out_h)
}

pub fn compute_dest_rect(
    img_w: u32,
    img_h: u32,
    screen_w: u32,
    screen_h: u32,
    _mat: &MatMode,
) -> (f32, f32, f32, f32) {
    let iw = img_w as f32;
    let ih = img_h as f32;
    let sw = screen_w as f32;
    let sh = screen_h as f32;
    let scale = (sw / iw).min(sh / ih);
    let w = iw * scale;
    let h = ih * scale;
    let x = (sw - w) * 0.5;
    let y = (sh - h) * 0.5;
    (x, y, w, h)
}

fn render_modes(gpu: &Gpu, img: &UploadedImage, alpha: f32) -> Result<()> {
    // Update uniforms
    let (x, y, w, h) = compute_dest_rect(img.w, img.h, gpu.target_w, gpu.target_h, &img.mat);
    let uni = Uniforms {
        screen_w: gpu.target_w as f32,
        screen_h: gpu.target_h as f32,
        dest_x: x,
        dest_y: y,
        dest_w: w,
        dest_h: h,
        alpha,
        _pad: [0.0; 3],
    };
    gpu.queue
        .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));

    // Choose clear color by mat
    let (r, g, b) = match img.mat {
        MatMode::LetterboxBlack => (0.0, 0.0, 0.0),
        MatMode::StudioMat { color_rgb, .. } => (
            (color_rgb.0 as f32) / 255.0,
            (color_rgb.1 as f32) / 255.0,
            (color_rgb.2 as f32) / 255.0,
        ),
        MatMode::BlurredBackground { .. } => (0.0, 0.0, 0.0),
    };

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-encoder"),
        });

    if let MatMode::BlurredBackground { strength, dim } = img.mat {
        // Downsampled multi-pass blur for background, then draw image in dest_rect.
        let passes = strength.clamp(2.0, 4.0).round() as u32;
        let down = match passes { 0 | 1 => 2, 2 => 4, 3 => 8, _ => 8 };
        let work_w = (gpu.target_w / down.max(1)).max(1);
        let work_h = (gpu.target_h / down.max(1)).max(1);

        let make_temp = |label: &str| -> (wgpu::Texture, wgpu::TextureView, wgpu::BindGroup) {
            let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: work_w, height: work_h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("{}-bind", label)),
                layout: &gpu.img_bind_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
                ],
            });
            (tex, view, bind)
        };

        let (_tex_a, view_a, bind_a) = make_temp("blur-a");
        let (_tex_b, view_b, bind_b) = make_temp("blur-b");

        // Helper to run a blur pass: dst <- blur(src). For blur shader, set uniforms to source texture size.
        let mut run_pass = |dst_view: &wgpu::TextureView, src_bind: &wgpu::BindGroup, src_w: u32, src_h: u32, clear: Option<wgpu::Color>| {
            let uni_blur = Uniforms { screen_w: src_w as f32, screen_h: src_h as f32, dest_x: 0.0, dest_y: 0.0, dest_w: 0.0, dest_h: 0.0, alpha: 0.0, _pad: [0.0;3] };
            gpu.queue.write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni_blur));
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: clear.map(wgpu::LoadOp::Clear).unwrap_or(wgpu::LoadOp::Load), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            rp.set_pipeline(&gpu.blur_pipeline);
            rp.set_bind_group(0, &gpu.uniform_bind, &[]);
            rp.set_bind_group(1, src_bind, &[]);
            rp.draw(0..3, 0..1);
        };

        // First pass: downsample + blur from original image into A
        run_pass(&view_a, &img.bind, img.w, img.h, Some(wgpu::Color { r: r as f64, g: g as f64, b: b as f64, a: 1.0 }));

        // Additional passes: ping-pong between A and B at low resolution
        let mut from_a = true;
        for _ in 1..passes {
            if from_a {
                run_pass(&view_b, &bind_a, work_w, work_h, None);
            } else {
                run_pass(&view_a, &bind_b, work_w, work_h, None);
            }
            from_a = !from_a;
        }

        // Final composite to full target: sample blurred low-res texture and dim.
        let (final_bind, final_w, final_h) = if from_a { (&bind_a, work_w, work_h) } else { (&bind_b, work_w, work_h) };
        let uni_bg = Uniforms { screen_w: final_w as f32, screen_h: final_h as f32, dest_x: 0.0, dest_y: 0.0, dest_w: 0.0, dest_h: 0.0, alpha: dim, _pad: [0.0;3] };
        gpu.queue.write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni_bg));
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-compose"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.target,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            rpass.set_pipeline(&gpu.blur_pipeline);
            rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
            rpass.set_bind_group(1, final_bind, &[]);
            rpass.draw(0..3, 0..1);
        }

        // Overlay the image in dest rect with full alpha
        let uni_img = Uniforms { alpha: 1.0, ..uni };
        gpu.queue.write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni_img));
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img-over-blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        rpass.set_pipeline(&gpu.img_pipeline);
        rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
        rpass.set_bind_group(1, &img.bind, &[]);
        rpass.draw(0..6, 0..1);
    } else {
        // Single pass: clear then draw image in dest rect
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: (r as f64),
                        g: (g as f64),
                        b: (b as f64),
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        rpass.set_pipeline(&gpu.img_pipeline);
        rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
        rpass.set_bind_group(1, &img.bind, &[]);
        rpass.draw(0..6, 0..1);
    }

    gpu.queue.submit(Some(encoder.finish()));
    Ok(())
}

async fn crossfade(
    gpu: &Gpu,
    cur: &UploadedImage,
    next: &UploadedImage,
    fade: Duration,
) -> Result<()> {
    let start = Instant::now();
    let mut t;
    loop {
        let elapsed = Instant::now() - start;
        if elapsed >= fade {
            // final frame: next = 1.0
            render_modes(gpu, next, 1.0)?;
            break;
        }
        t = elapsed.as_secs_f32() / fade.as_secs_f32();
        // render cur with alpha (1-t), then next with t
        render_blend_pair(gpu, cur, 1.0 - t, next, t)?;
        sleep(Duration::from_millis(16)).await; // ~60fps
    }
    Ok(())
}

fn render_blend_pair(
    gpu: &Gpu,
    a: &UploadedImage,
    a_alpha: f32,
    b: &UploadedImage,
    b_alpha: f32,
) -> Result<()> {
    // compute rect for b (used for both; assume same rect logic)
    let (x, y, w, h) = compute_dest_rect(b.w, b.h, gpu.target_w, gpu.target_h, &b.mat);
    let uni = Uniforms {
        screen_w: gpu.target_w as f32,
        screen_h: gpu.target_h as f32,
        dest_x: x,
        dest_y: y,
        dest_w: w,
        dest_h: h,
        alpha: a_alpha,
        _pad: [0.0; 3],
    };
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("fade-encoder"),
        });
    gpu.queue
        .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
    {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fade-pass-a"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        rpass.set_pipeline(&gpu.img_pipeline);
        rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
        rpass.set_bind_group(1, &a.bind, &[]);
        rpass.draw(0..6, 0..1);
    }
    let uni_b = Uniforms {
        alpha: b_alpha,
        ..uni
    };
    gpu.queue
        .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni_b));
    {
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fade-pass-b"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.target,
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
        rpass.set_pipeline(&gpu.img_pipeline);
        rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
        rpass.set_bind_group(1, &b.bind, &[]);
        rpass.draw(0..6, 0..1);
    }
    gpu.queue.submit(Some(encoder.finish()));
    Ok(())
}
