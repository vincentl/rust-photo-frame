use crate::events::{Displayed, MatMode, PhotoLoaded, PreparedImageCpu};
use anyhow::Result;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::debug;

const SCREEN_W: u32 = 1920;
const SCREEN_H: u32 = 1080;
const FADE_MS: u64 = 400; // crossfade duration
const DWELL_MS: u64 = 1800; // time to sit on a slide after fade

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: wgpu::TextureView,
    target_tex: wgpu::Texture,
    sampler: wgpu::Sampler,
    img_pipeline: wgpu::RenderPipeline,
    blur_pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    img_bind_layout: wgpu::BindGroupLayout,
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
    cancel: CancellationToken,
) -> Result<()> {
    let gpu = init_gpu().await?;

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
        }
    }
    Ok(())
}

async fn init_gpu() -> Result<Gpu> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .ok_or_else(|| anyhow::anyhow!("no suitable GPU adapter found"))?;
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("viewer-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await?;

    // Offscreen target
    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen"),
        size: wgpu::Extent3d {
            width: SCREEN_W,
            height: SCREEN_H,
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
        sampler,
        img_pipeline,
        blur_pipeline,
        uniform_buf,
        uniform_bind,
        img_bind_layout,
    })
}

fn upload_image(gpu: &Gpu, img: &PreparedImageCpu) -> Result<UploadedImage> {
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("photo-texture"),
        size: wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img.pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4 * img.width),
            rows_per_image: Some(img.height),
        },
        wgpu::Extent3d {
            width: img.width,
            height: img.height,
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
        w: img.width,
        h: img.height,
        mat: img.mat.clone(),
        path: img.path.clone(),
    })
}

pub(crate) fn compute_dest_rect(
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
    let (x, y, w, h) = compute_dest_rect(img.w, img.h, SCREEN_W, SCREEN_H, &img.mat);
    let uni = Uniforms {
        screen_w: SCREEN_W as f32,
        screen_h: SCREEN_H as f32,
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

    if let MatMode::BlurredBackground { strength: _, dim } = img.mat {
        // Pass 1: blur fullscreen background from the image itself
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blur-pass"),
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
            rpass.set_pipeline(&gpu.blur_pipeline);
            rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
            rpass.set_bind_group(1, &img.bind, &[]);
            // dimming factor is passed via alpha in this simple pipeline
            // draw full-screen triangle
            rpass.draw(0..3, 0..1);
            drop(rpass);
        }
        // Overlay the image in dest rect with full alpha (handled via uniform alpha)
        let uni = Uniforms {
            alpha: 1.0 - dim,
            ..uni
        };
        gpu.queue
            .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("img-over-blur"),
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
    let (x, y, w, h) = compute_dest_rect(b.w, b.h, SCREEN_W, SCREEN_H, &b.mat);
    let uni = Uniforms {
        screen_w: SCREEN_W as f32,
        screen_h: SCREEN_H as f32,
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
