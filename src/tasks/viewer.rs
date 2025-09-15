use crate::events::{Displayed, PhotoLoaded};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use std::collections::VecDeque;
use tracing::{debug, info};

pub fn run_windowed(
    from_loader: Receiver<PhotoLoaded>,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
    cfg: crate::config::Configuration,
) -> anyhow::Result<()> {
    use std::sync::Arc;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Window, WindowId};

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Uniforms {
        screen_w: f32,
        screen_h: f32,
        dest_x: f32,
        dest_y: f32,
        dest_w: f32,
        dest_h: f32,
        alpha: f32,
        _pad: [f32; 3],
    }

    struct GpuCtx {
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        limits: wgpu::Limits,
        uniform_buf: wgpu::Buffer,
        uniform_bind: wgpu::BindGroup,
        img_bind_layout: wgpu::BindGroupLayout,
        sampler: wgpu::Sampler,
        pipeline: wgpu::RenderPipeline,
        loading: Option<(wgpu::BindGroup, u32, u32)>,
    }

    struct ImgTex {
        bind: wgpu::BindGroup,
        w: u32,
        h: u32,
        path: std::path::PathBuf,
    }

    struct App {
        from_loader: Receiver<PhotoLoaded>,
        to_manager_displayed: Sender<Displayed>,
        cancel: CancellationToken,
        window: Option<Arc<Window>>,
        gpu: Option<GpuCtx>,
        current: Option<ImgTex>,
        next: Option<ImgTex>,
        fade_start: Option<std::time::Instant>,
        fade_ms: u64,
        displayed_at: Option<std::time::Instant>,
        dwell_ms: u64,
        pending: VecDeque<ImgTex>,
        preload_count: usize,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let window = Arc::new(
                event_loop
                    .create_window(Window::default_attributes().with_title("Photo Frame"))
                    .unwrap(),
            );
            // Enter borderless fullscreen and hide cursor for a clean demo
            use winit::window::Fullscreen;
            if let Some(m) = window.current_monitor() {
                window.set_fullscreen(Some(Fullscreen::Borderless(Some(m))));
            } else {
                window.set_fullscreen(Some(Fullscreen::Borderless(None)));
            }
            window.set_cursor_visible(false);

            let instance = wgpu::Instance::default();
            let surface = instance.create_surface(window.clone()).unwrap();
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                }))
                .unwrap();
            let limits = adapter.limits();
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("viewer-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits.clone(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::default(),
                }))
                .unwrap();
            let caps = surface.get_capabilities(&adapter);
            let format = caps
                .formats
                .iter()
                .copied()
                .find(|f| f.is_srgb())
                .unwrap_or(caps.formats[0]);
            let size = window.inner_size();
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode: wgpu::PresentMode::AutoVsync,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };
            surface.configure(&device, &config);
            // Resources for quad
            let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("viewer-uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uniform_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            let img_bind_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("linear-clamp"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("viewer-quad"),
                source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                    "shaders/viewer_quad.wgsl"
                ))),
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pipeline-layout"),
                bind_group_layouts: &[&uniform_layout, &img_bind_layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("quad-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState::default(),
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
            // Try to create a loading overlay texture from embedded PNG; fallback to 1x1 white
            let mut loading: Option<(wgpu::BindGroup, u32, u32)> = None;
            let loading_png: &[u8] = include_bytes!("../../assets/ui/loading.png");
            let loading_rgba = image::load_from_memory(loading_png)
                .ok()
                .map(|dynimg| dynimg.to_rgba8());
            if let Some(img) = loading_rgba {
                let lw = img.width();
                let lh = img.height();
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("loading-texture"),
                    size: wgpu::Extent3d {
                        width: lw,
                        height: lh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let bytes = img.as_raw();
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * lw),
                        rows_per_image: Some(lh),
                    },
                    wgpu::Extent3d {
                        width: lw,
                        height: lh,
                        depth_or_array_layers: 1,
                    },
                );
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("loading-bind"),
                    layout: &img_bind_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                    ],
                });
                loading = Some((bind, lw, lh));
            } else {
                let lw = 1u32;
                let lh = 1u32;
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("loading-pixel"),
                    size: wgpu::Extent3d {
                        width: lw,
                        height: lh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let white = [255u8, 255, 255, 255];
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &white,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4),
                        rows_per_image: Some(1),
                    },
                    wgpu::Extent3d {
                        width: lw,
                        height: lh,
                        depth_or_array_layers: 1,
                    },
                );
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("loading-bind"),
                    layout: &img_bind_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                    ],
                });
                loading = Some((bind, lw, lh));
            }

            self.window = Some(window);
            self.gpu = Some(GpuCtx {
                device,
                queue,
                surface,
                config,
                limits,
                uniform_buf,
                uniform_bind,
                img_bind_layout,
                sampler,
                pipeline,
                loading,
            });
            self.current = None;
        }

        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            window_id: WindowId,
            event: WindowEvent,
        ) {
            let Some(window) = self.window.as_ref() else {
                return;
            };
            if window.id() != window_id {
                return;
            }
            let Some(gpu) = self.gpu.as_mut() else {
                return;
            };
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(new_size) => {
                    gpu.config.width = new_size.width.max(1);
                    gpu.config.height = new_size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
                WindowEvent::ScaleFactorChanged { .. } => {}
                WindowEvent::RedrawRequested => {
                    let frame = match gpu.surface.get_current_texture() {
                        Ok(frame) => frame,
                        Err(err) => {
                            match err {
                                wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                                    gpu.surface.configure(&gpu.device, &gpu.config);
                                }
                                wgpu::SurfaceError::Timeout => {}
                                wgpu::SurfaceError::OutOfMemory => event_loop.exit(),
                                wgpu::SurfaceError::Other => {}
                            }
                            return;
                        }
                    };
                    let view = frame
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    let mut encoder =
                        gpu.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("draw-encoder"),
                            });
                    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("draw-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
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
                    rpass.set_pipeline(&gpu.pipeline);
                    rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
                    if let Some(start) = self.fade_start {
                        let mut t = ((start.elapsed().as_millis() as f32) / (self.fade_ms as f32).max(1.0)).clamp(0.0, 1.0);
                        t = t * t * (3.0 - 2.0 * t);
                        if let Some(cur) = &self.current {
                            let (dx, dy, dw, dh) = compute_dest_rect(
                                cur.w,
                                cur.h,
                                gpu.config.width,
                                gpu.config.height,
                            );
                            let uni = Uniforms {
                                screen_w: gpu.config.width as f32,
                                screen_h: gpu.config.height as f32,
                                dest_x: dx,
                                dest_y: dy,
                                dest_w: dw,
                                dest_h: dh,
                                alpha: 1.0 - t,
                                _pad: [0.0; 3],
                            };
                            gpu.queue
                                .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
                            rpass.set_bind_group(1, &cur.bind, &[]);
                            rpass.draw(0..6, 0..1);
                        }
                        if let Some(next) = &self.next {
                            let (dx, dy, dw, dh) = compute_dest_rect(
                                next.w,
                                next.h,
                                gpu.config.width,
                                gpu.config.height,
                            );
                            let uni = Uniforms {
                                screen_w: gpu.config.width as f32,
                                screen_h: gpu.config.height as f32,
                                dest_x: dx,
                                dest_y: dy,
                                dest_w: dw,
                                dest_h: dh,
                                alpha: t,
                                _pad: [0.0; 3],
                            };
                            gpu.queue
                                .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
                            rpass.set_bind_group(1, &next.bind, &[]);
                            rpass.draw(0..6, 0..1);
                        }
                        if t >= 1.0 {
                            if let Some(next) = self.next.take() {
                                let path = next.path.clone();
                                self.current = Some(next);
                                self.fade_start = None;
                                self.displayed_at = Some(std::time::Instant::now());
                                info!("transition_end path={} queue_depth={}", path.display(), self.pending.len());
                                let _ = self.to_manager_displayed.try_send(Displayed(path));
                            } else {
                                self.fade_start = None;
                            }
                        }
                    } else if let Some(cur) = &self.current {
                        let (dx, dy, dw, dh) =
                            compute_dest_rect(cur.w, cur.h, gpu.config.width, gpu.config.height);
                        let uni = Uniforms {
                            screen_w: gpu.config.width as f32,
                            screen_h: gpu.config.height as f32,
                            dest_x: dx,
                            dest_y: dy,
                            dest_w: dw,
                            dest_h: dh,
                            alpha: 1.0,
                            _pad: [0.0; 3],
                        };
                        gpu.queue
                            .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
                        rpass.set_bind_group(1, &cur.bind, &[]);
                        rpass.draw(0..6, 0..1);
                    } else if let Some((bind, lw, lh)) = &gpu.loading {
                        // Draw loading overlay centered scaled to a reasonable fraction
                        let sw = gpu.config.width as f32;
                        let sh = gpu.config.height as f32;
                        let iw = *lw as f32;
                        let ih = *lh as f32;
                        let maxw = sw * 0.4;
                        let maxh = sh * 0.2;
                        let scale = (maxw / iw).min(maxh / ih).min(1.0);
                        let dw = (iw * scale).clamp(0.0, sw);
                        let dh = (ih * scale).clamp(0.0, sh);
                        let dx = (sw - dw) * 0.5;
                        let dy = (sh - dh) * 0.5;
                        let uni = Uniforms {
                            screen_w: sw,
                            screen_h: sh,
                            dest_x: dx,
                            dest_y: dy,
                            dest_w: dw,
                            dest_h: dh,
                            alpha: 1.0,
                            _pad: [0.0; 3],
                        };
                        gpu.queue
                            .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
                        rpass.set_bind_group(1, bind, &[]);
                        rpass.draw(0..6, 0..1);
                    }
                    drop(rpass);

                    gpu.queue.submit(Some(encoder.finish()));
                    frame.present();
                }
                _ => {}
            }
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.cancel.is_cancelled() {
                event_loop.exit();
                return;
            }
            // Pull from loader only when there is capacity to avoid dropping
            // pending images; this allows the mpsc channel to provide backpressure.
            while self.pending.len() < self.preload_count {
                let Ok(PhotoLoaded(img)) = self.from_loader.try_recv() else { break };
                if let Some(gpu) = self.gpu.as_ref() {
                    let (out_w, out_h) = compute_scaled_size(
                        img.width,
                        img.height,
                        gpu.config.width,
                        gpu.config.height,
                        1.0,
                        gpu.limits.max_texture_dimension_2d,
                    );
                    let mut pixels: std::borrow::Cow<'_, [u8]> =
                        std::borrow::Cow::Borrowed(&img.pixels);
                    if out_w != img.width || out_h != img.height {
                        if let Some(src) =
                            image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone())
                        {
                            let resized = image::imageops::resize(
                                &src,
                                out_w,
                                out_h,
                                image::imageops::FilterType::Triangle,
                            );
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
                    let stride = 4 * out_w;
                    let padded = compute_padded_stride(stride);
                    let upload: std::borrow::Cow<'_, [u8]> = if padded != stride {
                        let mut staging = vec![0u8; (padded as usize) * (out_h as usize)];
                        let rs = stride as usize;
                        let rd = padded as usize;
                        let src = pixels.as_ref();
                        for y in 0..(out_h as usize) {
                            let so = y * rs;
                            let doff = y * rd;
                            staging[doff..doff + rs].copy_from_slice(&src[so..so + rs]);
                        }
                        std::borrow::Cow::Owned(staging)
                    } else {
                        pixels
                    };
                    gpu.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        &upload,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(padded),
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
                    // Queue decoded image for later consumption
                    let new_tex = ImgTex { bind, w: out_w, h: out_h, path: img.path };
                    self.pending.push_back(new_tex);
                    debug!("queued_image depth={}", self.pending.len());
                    // If nothing showing yet, promote immediately
                    if self.current.is_none() && self.fade_start.is_none() {
                        if let Some(first) = self.pending.pop_front() {
                            info!("first_image path={}", first.path.display());
                            self.current = Some(first);
                            self.displayed_at = Some(std::time::Instant::now());
                            if let Some(cur) = &self.current {
                                let _ = self.to_manager_displayed.try_send(Displayed(cur.path.clone()));
                            }
                        }
                    }
                }
            }
            // If dwell elapsed and we have pending, stage next and start fade
            if self.fade_start.is_none() {
                if let Some(shown_at) = self.displayed_at {
                    if shown_at.elapsed() >= std::time::Duration::from_millis(self.dwell_ms) {
                        if self.next.is_none() {
                            if let Some(stage) = self.pending.pop_front() {
                                info!("transition_start path={} queue_depth={}", stage.path.display(), self.pending.len());
                                self.next = Some(stage);
                            }
                        }
                        if self.next.is_some() {
                            self.fade_start = Some(std::time::Instant::now());
                        }
                    }
                }
            }
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    let event_loop = EventLoop::new()?;
    let mut app = App {
        from_loader,
        to_manager_displayed,
        cancel,
        window: None,
        gpu: None,
        current: None,
        next: None,
        fade_start: None,
        fade_ms: cfg.fade_ms,
        displayed_at: None,
        dwell_ms: cfg.dwell_ms,
        pending: VecDeque::new(),
        preload_count: cfg.preload_count,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn compute_padded_stride(bytes_per_row: u32) -> u32 {
    const ALIGN: u32 = 256;
    if bytes_per_row == 0 {
        return 0;
    }
    bytes_per_row.div_ceil(ALIGN) * ALIGN
}

fn compute_scaled_size(
    img_w: u32,
    img_h: u32,
    surf_w: u32,
    surf_h: u32,
    oversample: f32,
    max_dim: u32,
) -> (u32, u32) {
    if img_w == 0 || img_h == 0 || surf_w == 0 || surf_h == 0 {
        return (1, 1);
    }
    let max_w = ((surf_w as f32) * oversample).round().max(1.0) as u32;
    let max_h = ((surf_h as f32) * oversample).round().max(1.0) as u32;
    let max_w = max_w.min(max_dim).max(1);
    let max_h = max_h.min(max_dim).max(1);
    let sw = (max_w as f32) / (img_w as f32);
    let sh = (max_h as f32) / (img_h as f32);
    let s = sw.min(sh).min(1.0);
    let out_w = ((img_w as f32) * s).floor().max(1.0) as u32;
    let out_h = ((img_h as f32) * s).floor().max(1.0) as u32;
    (out_w, out_h)
}

fn compute_dest_rect(img_w: u32, img_h: u32, screen_w: u32, screen_h: u32) -> (f32, f32, f32, f32) {
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
