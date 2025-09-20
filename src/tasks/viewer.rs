use crate::config::{FixedImageFit, MattingMode, MattingOptions};
use crate::events::{Displayed, PhotoLoaded, PreparedImageCpu};
use crate::processing::blur::apply_blur;
use crossbeam_channel::{bounded, Receiver as CbReceiver, Sender as CbSender, TrySendError};
use image::{imageops, Rgba, RgbaImage};
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

pub fn run_windowed(
    from_loader: Receiver<PhotoLoaded>,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
    cfg: crate::config::Configuration,
) -> anyhow::Result<()> {
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Fullscreen, Window, WindowId};

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

    struct TexturePlane {
        bind: wgpu::BindGroup,
        w: u32,
        h: u32,
    }

    struct ImgTex {
        plane: TexturePlane,
        path: std::path::PathBuf,
    }

    #[derive(Clone)]
    struct MatParams {
        screen_w: u32,
        screen_h: u32,
        oversample: f32,
        max_dim: u32,
        matting: MattingOptions,
    }

    struct MatTask {
        image: PreparedImageCpu,
        params: MatParams,
    }

    struct ImagePlane {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    }

    struct MatResult {
        path: std::path::PathBuf,
        canvas: ImagePlane,
    }

    struct MattingPipeline {
        task_tx: CbSender<MatTask>,
        result_rx: CbReceiver<MatResult>,
    }

    impl MattingPipeline {
        fn new(worker_count: usize, capacity: usize) -> Self {
            let worker_count = worker_count.max(1);
            let capacity = capacity.max(worker_count).max(2);
            let (task_tx, task_rx) = bounded::<MatTask>(capacity);
            let (result_tx, result_rx) = bounded::<MatResult>(capacity);
            let task_rx = Arc::new(task_rx);
            let result_tx = Arc::new(result_tx);
            for _ in 0..worker_count {
                let task_rx = Arc::clone(&task_rx);
                let result_tx = Arc::clone(&result_tx);
                std::thread::spawn(move || {
                    while let Ok(task) = task_rx.recv() {
                        if let Some(result) = process_mat_task(task) {
                            if result_tx.send(result).is_err() {
                                break;
                            }
                        }
                    }
                });
            }
            Self { task_tx, result_rx }
        }

        fn try_submit(&self, task: MatTask) -> Result<(), MatTask> {
            match self.task_tx.try_send(task) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(task)) | Err(TrySendError::Disconnected(task)) => Err(task),
            }
        }

        fn try_recv(&self) -> Option<MatResult> {
            self.result_rx.try_recv().ok()
        }
    }

    fn process_mat_task(task: MatTask) -> Option<MatResult> {
        let MatTask { image, params } = task;
        let PreparedImageCpu {
            path,
            width,
            height,
            pixels,
        } = image;
        if width == 0 || height == 0 {
            return None;
        }
        let src = RgbaImage::from_raw(width, height, pixels)?;
        let MatParams {
            screen_w,
            screen_h,
            oversample,
            max_dim,
            matting,
        } = params;
        if screen_w == 0 || screen_h == 0 {
            return None;
        }

        let (canvas_w, canvas_h) = compute_canvas_size(screen_w, screen_h, oversample, max_dim);
        let margin = (matting.minimum_mat_percentage / 100.0).clamp(0.0, 0.45);
        let max_upscale = matting.max_upscale_factor.max(1.0);
        let (final_w, final_h) =
            resize_to_fit_with_margin(canvas_w, canvas_h, width, height, margin, max_upscale);
        let (offset_x, offset_y) = center_offset(final_w, final_h, canvas_w, canvas_h);

        let mut background = match matting.style {
            MattingMode::FixedColor { color } => {
                let px = Rgba([color[0], color[1], color[2], 255]);
                RgbaImage::from_pixel(canvas_w, canvas_h, px)
            }
            MattingMode::Blur {
                sigma,
                max_sample_dim,
                backend,
            } => {
                let (bg_w, bg_h) = resize_to_cover(canvas_w, canvas_h, width, height, max_dim);
                let mut bg = imageops::resize(&src, bg_w, bg_h, imageops::FilterType::Triangle);
                if bg_w > canvas_w || bg_h > canvas_h {
                    let crop_x = (bg_w.saturating_sub(canvas_w)) / 2;
                    let crop_y = (bg_h.saturating_sub(canvas_h)) / 2;
                    bg = imageops::crop_imm(&bg, crop_x, crop_y, canvas_w, canvas_h).to_image();
                } else if bg_w < canvas_w || bg_h < canvas_h {
                    let mut canvas =
                        RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0u8, 0, 0, 255]));
                    let (bg_x, bg_y) = center_offset(bg_w, bg_h, canvas_w, canvas_h);
                    imageops::overlay(&mut canvas, &bg, bg_x as i64, bg_y as i64);
                    bg = canvas;
                }
                if sigma > 0.0 {
                    let limit = max_sample_dim
                        .filter(|v| *v > 0)
                        .unwrap_or_else(|| {
                            #[cfg(target_arch = "aarch64")]
                            {
                                MattingMode::default_blur_max_sample_dim()
                            }
                            #[cfg(not(target_arch = "aarch64"))]
                            {
                                max_dim
                            }
                        })
                        .min(max_dim)
                        .max(1);

                    let mut sample = bg;
                    let mut sigma_px = sigma;
                    let canvas_max = canvas_w.max(canvas_h).max(1);
                    if canvas_max > limit {
                        let scale = (limit as f32) / (canvas_max as f32);
                        let sample_w =
                            ((canvas_w as f32) * scale).round().clamp(1.0, limit as f32) as u32;
                        let sample_h =
                            ((canvas_h as f32) * scale).round().clamp(1.0, limit as f32) as u32;
                        sample = imageops::resize(
                            &sample,
                            sample_w,
                            sample_h,
                            imageops::FilterType::Triangle,
                        );
                        sigma_px *= scale.max(0.01);
                    }

                    let mut blurred: RgbaImage = apply_blur(&sample, sigma_px, backend);
                    if blurred.width() != canvas_w || blurred.height() != canvas_h {
                        blurred = imageops::resize(
                            &blurred,
                            canvas_w,
                            canvas_h,
                            imageops::FilterType::CatmullRom,
                        );
                    }
                    blurred
                } else {
                    bg
                }
            }
            MattingMode::Studio {
                bevel_width,
                highlight_strength,
                shadow_strength,
                texture_strength,
            } => {
                let avg = average_color(&src);
                render_studio_mat(
                    path.as_path(),
                    width,
                    height,
                    canvas_w,
                    canvas_h,
                    offset_x,
                    offset_y,
                    final_w,
                    final_h,
                    avg,
                    bevel_width,
                    highlight_strength,
                    shadow_strength,
                    texture_strength,
                )
            }
            MattingMode::FixedImage { fit, .. } => {
                if let Some(bg) = matting.runtime.fixed_image.as_ref() {
                    let bg_img: &RgbaImage = bg.as_ref();
                    match fit {
                        FixedImageFit::Stretch => imageops::resize(
                            bg_img,
                            canvas_w,
                            canvas_h,
                            imageops::FilterType::CatmullRom,
                        ),
                        FixedImageFit::Cover => {
                            let (bg_w, bg_h) = resize_to_cover(
                                canvas_w,
                                canvas_h,
                                bg_img.width(),
                                bg_img.height(),
                                max_dim,
                            );
                            let mut resized = imageops::resize(
                                bg_img,
                                bg_w,
                                bg_h,
                                imageops::FilterType::CatmullRom,
                            );
                            if bg_w > canvas_w || bg_h > canvas_h {
                                let crop_x = (bg_w.saturating_sub(canvas_w)) / 2;
                                let crop_y = (bg_h.saturating_sub(canvas_h)) / 2;
                                resized = imageops::crop_imm(
                                    &resized, crop_x, crop_y, canvas_w, canvas_h,
                                )
                                .to_image();
                            } else if bg_w < canvas_w || bg_h < canvas_h {
                                let mut canvas = RgbaImage::from_pixel(
                                    canvas_w,
                                    canvas_h,
                                    average_color_rgba(bg_img),
                                );
                                let (ox, oy) = center_offset(bg_w, bg_h, canvas_w, canvas_h);
                                imageops::overlay(&mut canvas, &resized, ox as i64, oy as i64);
                                resized = canvas;
                            }
                            resized
                        }
                        FixedImageFit::Contain => {
                            let (bg_w, bg_h) = resize_to_contain(
                                canvas_w,
                                canvas_h,
                                bg_img.width(),
                                bg_img.height(),
                                max_dim,
                            );
                            let resized = imageops::resize(
                                bg_img,
                                bg_w,
                                bg_h,
                                imageops::FilterType::CatmullRom,
                            );
                            let mut canvas = RgbaImage::from_pixel(
                                canvas_w,
                                canvas_h,
                                average_color_rgba(bg_img),
                            );
                            let (ox, oy) = center_offset(bg_w, bg_h, canvas_w, canvas_h);
                            imageops::overlay(&mut canvas, &resized, ox as i64, oy as i64);
                            canvas
                        }
                    }
                } else {
                    RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 255]))
                }
            }
        };

        let main_img = if final_w == width && final_h == height {
            src
        } else {
            imageops::resize(&src, final_w, final_h, imageops::FilterType::Triangle)
        };
        imageops::overlay(&mut background, &main_img, offset_x as i64, offset_y as i64);

        let canvas = ImagePlane {
            width: canvas_w,
            height: canvas_h,
            pixels: background.into_raw(),
        };

        Some(MatResult { path, canvas })
    }

    fn upload_plane(gpu: &GpuCtx, plane: ImagePlane) -> Option<TexturePlane> {
        let ImagePlane {
            width,
            height,
            pixels,
        } = plane;
        if width == 0 || height == 0 {
            return None;
        }
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("photo-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let stride = 4 * width;
        let padded = compute_padded_stride(stride);
        let upload: Cow<'_, [u8]> = if padded != stride {
            let mut staging = vec![0u8; (padded as usize) * (height as usize)];
            let rs = stride as usize;
            let rd = padded as usize;
            for y in 0..(height as usize) {
                let so = y * rs;
                let doff = y * rd;
                staging[doff..doff + rs].copy_from_slice(&pixels[so..so + rs]);
            }
            Cow::Owned(staging)
        } else {
            Cow::Borrowed(&pixels)
        };
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            upload.as_ref(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
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
        Some(TexturePlane {
            bind,
            w: width,
            h: height,
        })
    }

    fn upload_mat_result(gpu: &GpuCtx, result: MatResult) -> Option<ImgTex> {
        let MatResult { path, canvas } = result;
        let plane = upload_plane(gpu, canvas)?;
        Some(ImgTex { plane, path })
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
        oversample: f32,
        matting: MattingOptions,
        mat_pipeline: MattingPipeline,
        mat_inflight: usize,
        ready_results: VecDeque<MatResult>,
        deferred_images: VecDeque<PreparedImageCpu>,
        clear_color: wgpu::Color,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.pending.clear();
            self.ready_results.clear();
            self.deferred_images.clear();
            self.current = None;
            self.next = None;
            self.fade_start = None;
            self.displayed_at = None;
            self.mat_inflight = 0;
            let monitor = event_loop.primary_monitor();
            let attrs = Window::default_attributes()
                .with_title("Photo Frame")
                .with_decorations(false)
                .with_fullscreen(Some(match monitor {
                    Some(m) => Fullscreen::Borderless(Some(m)),
                    None => Fullscreen::Borderless(None),
                }));
            let window = Arc::new(event_loop.create_window(attrs).unwrap());
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
            let loading_png: &[u8] = include_bytes!("../../assets/ui/loading.png");
            let loading_rgba = image::load_from_memory(loading_png)
                .ok()
                .map(|dynimg| dynimg.to_rgba8());
            let loading = if let Some(img) = loading_rgba {
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
                Some((bind, lw, lh))
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
                Some((bind, lw, lh))
            };

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
                WindowEvent::ScaleFactorChanged {
                    mut inner_size_writer,
                    ..
                } => {
                    let size = window.inner_size();
                    let _ = inner_size_writer.request_inner_size(size);
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
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
                                load: wgpu::LoadOp::Clear(self.clear_color),
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
                        let mut t = ((start.elapsed().as_millis() as f32)
                            / (self.fade_ms as f32).max(1.0))
                        .clamp(0.0, 1.0);
                        t = t * t * (3.0 - 2.0 * t);
                        if let Some(cur) = &self.current {
                            let (dx, dy, dw, dh) = compute_cover_rect(
                                cur.plane.w,
                                cur.plane.h,
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
                            rpass.set_bind_group(1, &cur.plane.bind, &[]);
                            rpass.draw(0..6, 0..1);
                        }
                        if let Some(next) = &self.next {
                            let (dx, dy, dw, dh) = compute_cover_rect(
                                next.plane.w,
                                next.plane.h,
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
                            rpass.set_bind_group(1, &next.plane.bind, &[]);
                            rpass.draw(0..6, 0..1);
                        }
                        if t >= 1.0 {
                            if let Some(next) = self.next.take() {
                                let path = next.path.clone();
                                self.current = Some(next);
                                self.fade_start = None;
                                self.displayed_at = Some(std::time::Instant::now());
                                info!(
                                    "transition_end path={} queue_depth={}",
                                    path.display(),
                                    self.pending.len()
                                );
                                let _ = self.to_manager_displayed.try_send(Displayed(path));
                            } else {
                                self.fade_start = None;
                            }
                        }
                    } else if let Some(cur) = &self.current {
                        let (dx, dy, dw, dh) = compute_cover_rect(
                            cur.plane.w,
                            cur.plane.h,
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
                            alpha: 1.0,
                            _pad: [0.0; 3],
                        };
                        gpu.queue
                            .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uni));
                        rpass.set_bind_group(1, &cur.plane.bind, &[]);
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
            while let Some(result) = self.mat_pipeline.try_recv() {
                self.mat_inflight = self.mat_inflight.saturating_sub(1);
                self.ready_results.push_back(result);
            }
            if let Some(gpu) = self.gpu.as_ref() {
                while let Some(result) = self.ready_results.pop_front() {
                    if let Some(new_tex) = upload_mat_result(gpu, result) {
                        self.pending.push_back(new_tex);
                        debug!("queued_image depth={}", self.pending.len());
                    }
                }
            }
            while self.pending.len() + self.mat_inflight < self.preload_count {
                let next_img = if let Some(img) = self.deferred_images.pop_front() {
                    Some(img)
                } else {
                    match self.from_loader.try_recv() {
                        Ok(PhotoLoaded(img)) => Some(img),
                        Err(_) => None,
                    }
                };
                let Some(img) = next_img else {
                    break;
                };
                let Some(gpu) = self.gpu.as_ref() else {
                    self.deferred_images.push_front(img);
                    break;
                };
                let params = MatParams {
                    screen_w: gpu.config.width.max(1),
                    screen_h: gpu.config.height.max(1),
                    oversample: self.oversample,
                    max_dim: gpu.limits.max_texture_dimension_2d,
                    matting: self.matting.clone(),
                };
                let task = MatTask { image: img, params };
                match self.mat_pipeline.try_submit(task) {
                    Ok(()) => {
                        self.mat_inflight += 1;
                    }
                    Err(MatTask { image, .. }) => {
                        self.deferred_images.push_front(image);
                        break;
                    }
                }
            }
            if self.current.is_none() && self.fade_start.is_none() {
                if let Some(first) = self.pending.pop_front() {
                    info!("first_image path={}", first.path.display());
                    self.current = Some(first);
                    self.displayed_at = Some(std::time::Instant::now());
                    if let Some(cur) = &self.current {
                        let _ = self
                            .to_manager_displayed
                            .try_send(Displayed(cur.path.clone()));
                    }
                }
            }
            // If dwell elapsed and we have pending, stage next and start fade
            if self.fade_start.is_none() {
                if let Some(shown_at) = self.displayed_at {
                    if shown_at.elapsed() >= std::time::Duration::from_millis(self.dwell_ms) {
                        if self.next.is_none() {
                            if let Some(stage) = self.pending.pop_front() {
                                info!(
                                    "transition_start path={} queue_depth={}",
                                    stage.path.display(),
                                    self.pending.len()
                                );
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
    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .max(1);
    let pipeline_capacity = cfg.viewer_preload_count.max(2);
    let mat_pipeline = MattingPipeline::new(worker_count, pipeline_capacity);
    let clear_color = match &cfg.matting.style {
        MattingMode::FixedColor { color } => wgpu::Color {
            r: (color[0] as f64) / 255.0,
            g: (color[1] as f64) / 255.0,
            b: (color[2] as f64) / 255.0,
            a: 1.0,
        },
        MattingMode::Blur { .. } => wgpu::Color::BLACK,
        MattingMode::Studio { .. } => wgpu::Color::BLACK,
        MattingMode::FixedImage { .. } => wgpu::Color::BLACK,
    };
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
        preload_count: cfg.viewer_preload_count,
        oversample: cfg.oversample,
        matting: cfg.matting.clone(),
        mat_pipeline,
        mat_inflight: 0,
        ready_results: VecDeque::new(),
        deferred_images: VecDeque::new(),
        clear_color,
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

fn compute_canvas_size(screen_w: u32, screen_h: u32, oversample: f32, max_dim: u32) -> (u32, u32) {
    let sw = (screen_w as f32 * oversample)
        .round()
        .clamp(1.0, max_dim as f32);
    let sh = (screen_h as f32 * oversample)
        .round()
        .clamp(1.0, max_dim as f32);
    (sw as u32, sh as u32)
}

fn resize_to_fit_with_margin(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    margin_frac: f32,
    max_upscale: f32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f32;
    let ih = src_h.max(1) as f32;
    let cw = canvas_w.max(1) as f32;
    let ch = canvas_h.max(1) as f32;
    let margin_frac = margin_frac.clamp(0.0, 0.45);
    let avail_w = (cw * (1.0 - 2.0 * margin_frac)).max(1.0);
    let avail_h = (ch * (1.0 - 2.0 * margin_frac)).max(1.0);
    let max_upscale = max_upscale.max(1.0);
    let scale = (avail_w / iw).min(avail_h / ih).min(max_upscale);
    let w = (iw * scale).round().clamp(1.0, cw);
    let h = (ih * scale).round().clamp(1.0, ch);
    (w as u32, h as u32)
}

fn resize_to_cover(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    max_dim: u32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f32;
    let ih = src_h.max(1) as f32;
    let cw = canvas_w.max(1) as f32;
    let ch = canvas_h.max(1) as f32;
    let scale = (cw / iw).max(ch / ih).max(1.0);
    let w = (iw * scale).round().clamp(1.0, max_dim as f32);
    let h = (ih * scale).round().clamp(1.0, max_dim as f32);
    (w as u32, h as u32)
}

fn resize_to_contain(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    max_dim: u32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f32;
    let ih = src_h.max(1) as f32;
    let cw = canvas_w.max(1) as f32;
    let ch = canvas_h.max(1) as f32;
    let scale = (cw / iw).min(ch / ih).max(0.0);
    let scale = if scale.is_finite() { scale } else { 1.0 };
    let w = (iw * scale).round().clamp(1.0, max_dim as f32);
    let h = (ih * scale).round().clamp(1.0, max_dim as f32);
    (w as u32, h as u32)
}

fn center_offset(inner_w: u32, inner_h: u32, outer_w: u32, outer_h: u32) -> (u32, u32) {
    let ox = outer_w.saturating_sub(inner_w) / 2;
    let oy = outer_h.saturating_sub(inner_h) / 2;
    (ox, oy)
}

fn average_color(img: &RgbaImage) -> [f32; 3] {
    let mut accum = [0f64; 3];
    let mut total = 0f64;
    for pixel in img.pixels() {
        let alpha = (pixel[3] as f64) / 255.0;
        if alpha <= 0.0 {
            continue;
        }
        total += alpha;
        for c in 0..3 {
            accum[c] += (pixel[c] as f64) * alpha;
        }
    }
    if total <= f64::EPSILON {
        return [0.1, 0.1, 0.1];
    }
    [
        (accum[0] / (255.0 * total)) as f32,
        (accum[1] / (255.0 * total)) as f32,
        (accum[2] / (255.0 * total)) as f32,
    ]
}

fn average_color_rgba(img: &RgbaImage) -> Rgba<u8> {
    let avg = average_color(img);
    Rgba([
        (avg[0] * 255.0).round().clamp(0.0, 255.0) as u8,
        (avg[1] * 255.0).round().clamp(0.0, 255.0) as u8,
        (avg[2] * 255.0).round().clamp(0.0, 255.0) as u8,
        255,
    ])
}

fn render_studio_mat(
    path: &std::path::Path,
    src_w: u32,
    src_h: u32,
    canvas_w: u32,
    canvas_h: u32,
    inner_x: u32,
    inner_y: u32,
    inner_w: u32,
    inner_h: u32,
    avg_color: [f32; 3],
    bevel_width: f32,
    highlight_strength: f32,
    shadow_strength: f32,
    texture_strength: f32,
) -> RgbaImage {
    let highlight_strength = highlight_strength.clamp(0.0, 1.5);
    let shadow_strength = shadow_strength.clamp(0.0, 1.5);
    let texture_strength = texture_strength.clamp(0.0, 1.0);

    let seed = studio_seed(
        path, src_w, src_h, canvas_w, canvas_h, inner_x, inner_y, inner_w, inner_h,
    );
    let coarse_seed = seed;
    let fine_seed = seed.rotate_left(17) ^ 0xa076_1d64_78bd_642f;
    let fiber_seed = seed.rotate_left(29) ^ 0xe703_7ed1_a0b4_28db;

    let inner_right = inner_x.saturating_add(inner_w);
    let inner_bottom = inner_y.saturating_add(inner_h);
    let left_border = inner_x as f32;
    let top_border = inner_y as f32;
    let right_border = canvas_w.saturating_sub(inner_right) as f32;
    let bottom_border = canvas_h.saturating_sub(inner_bottom) as f32;
    let available = left_border
        .min(top_border)
        .min(right_border)
        .min(bottom_border);
    let bevel_ratio = (bevel_width.max(0.0)) / 100.0;
    let mut bevel_px = (canvas_w.min(canvas_h) as f32) * bevel_ratio;
    bevel_px = bevel_px.max(1.0).min(available.max(1.0));
    let inv_bevel = 1.0 / bevel_px.max(1.0);

    let center_x = (canvas_w as f32) * 0.5;
    let center_y = (canvas_h as f32) * 0.5;

    let luma = (avg_color[0] * 0.299 + avg_color[1] * 0.587 + avg_color[2] * 0.114).clamp(0.0, 1.0);
    let mut base = [0.0f32; 3];
    for (i, channel) in base.iter_mut().enumerate() {
        let softened = lerp(luma, avg_color[i], 0.6);
        *channel = lerp(softened, 0.92, 0.25).clamp(0.04, 0.98);
    }

    let mut mat = RgbaImage::new(canvas_w, canvas_h);
    for (x, y, pixel) in mat.enumerate_pixels_mut() {
        let px = x as f32 + 0.5;
        let py = y as f32 + 0.5;

        let dist_left = (inner_x as f32 - px).max(0.0);
        let dist_top = (inner_y as f32 - py).max(0.0);
        let dist_right = (px - inner_right as f32).max(0.0);
        let dist_bottom = (py - inner_bottom as f32).max(0.0);

        let left_term = ((bevel_px - dist_left).max(0.0) * inv_bevel).powf(1.6);
        let top_term = ((bevel_px - dist_top).max(0.0) * inv_bevel).powf(1.6);
        let right_term = ((bevel_px - dist_right).max(0.0) * inv_bevel).powf(1.6);
        let bottom_term = ((bevel_px - dist_bottom).max(0.0) * inv_bevel).powf(1.6);

        let highlight = ((left_term + top_term) * 0.5) * highlight_strength;
        let shadow = ((right_term + bottom_term) * 0.5) * shadow_strength;

        let inner_dist = dist_left.min(dist_top).min(dist_right).min(dist_bottom);
        let bevel_profile = ((bevel_px - inner_dist).max(0.0) * inv_bevel).powf(2.0);
        let bevel_pop = bevel_profile * 0.18 * (highlight_strength + shadow_strength).max(0.2);

        let diag = ((px - inner_x as f32) - (py - inner_y as f32))
            / ((inner_w.max(1) + inner_h.max(1)) as f32);
        let diag_emphasis = diag * 0.2 * (highlight_strength - shadow_strength);

        let mut shade = 1.0 + highlight - shadow + bevel_pop + diag_emphasis;
        let dx = (px - center_x) / (canvas_w.max(1) as f32);
        let dy = (py - center_y) / (canvas_h.max(1) as f32);
        let radial = (dx * dx + dy * dy).sqrt();
        shade *= 1.0 - radial.powf(1.35) * 0.05;
        let shade = shade.clamp(0.25, 3.0);

        let texture = if texture_strength > 0.0 {
            let coarse = fbm_noise(coarse_seed, px * 0.018, py * 0.018, 5);
            let fine = fbm_noise(fine_seed, px * 0.07, py * 0.07, 3);
            let fibers = fbm_noise(
                fiber_seed,
                px * 0.12 + py * 0.018,
                py * 0.12 - px * 0.018,
                3,
            );
            let combined = (coarse * 0.6 + fine * 0.4 - 0.5) * 0.55 + (fibers - 0.5) * 0.35;
            combined * texture_strength
        } else {
            0.0
        };

        for c in 0..3 {
            let mut value = base[c] * shade + texture;
            value = value.clamp(0.0, 1.0);
            pixel[c] = srgb_u8(value);
        }
        pixel[3] = 255;
    }

    mat
}

fn studio_seed(
    path: &std::path::Path,
    src_w: u32,
    src_h: u32,
    canvas_w: u32,
    canvas_h: u32,
    inner_x: u32,
    inner_y: u32,
    inner_w: u32,
    inner_h: u32,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    src_w.hash(&mut hasher);
    src_h.hash(&mut hasher);
    canvas_w.hash(&mut hasher);
    canvas_h.hash(&mut hasher);
    inner_x.hash(&mut hasher);
    inner_y.hash(&mut hasher);
    inner_w.hash(&mut hasher);
    inner_h.hash(&mut hasher);
    hasher.finish()
}

fn srgb_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hash_2d(seed: u64, x: i32, y: i32) -> f32 {
    let mut v = seed
        .wrapping_add((x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .wrapping_add((y as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9));
    v ^= v >> 30;
    v = v.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94d0_49bb_1331_11eb);
    v ^= v >> 31;
    (v as f64 / u64::MAX as f64) as f32
}

fn value_noise(seed: u64, x: f32, y: f32) -> f32 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let xf = x - x0 as f32;
    let yf = y - y0 as f32;
    let n00 = hash_2d(seed, x0, y0);
    let n10 = hash_2d(seed, x0 + 1, y0);
    let n01 = hash_2d(seed, x0, y0 + 1);
    let n11 = hash_2d(seed, x0 + 1, y0 + 1);
    let u = smoothstep(xf);
    let v = smoothstep(yf);
    let nx0 = lerp(n00, n10, u);
    let nx1 = lerp(n01, n11, u);
    lerp(nx0, nx1, v)
}

fn fbm_noise(seed: u64, x: f32, y: f32, octaves: u32) -> f32 {
    let mut frequency = 1.0f32;
    let mut amplitude = 1.0f32;
    let mut sum = 0.0f32;
    let mut total = 0.0f32;
    let mut cur_seed = seed;
    for _ in 0..octaves.max(1) {
        sum += value_noise(cur_seed, x * frequency, y * frequency) * amplitude;
        total += amplitude;
        frequency *= 2.0;
        amplitude *= 0.5;
        cur_seed = cur_seed
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(0x243f_6a88_85a3_08d3);
    }
    if total > f32::EPSILON {
        sum / total
    } else {
        0.5
    }
}

fn compute_cover_rect(
    img_w: u32,
    img_h: u32,
    screen_w: u32,
    screen_h: u32,
) -> (f32, f32, f32, f32) {
    let iw = img_w.max(1) as f32;
    let ih = img_h.max(1) as f32;
    let sw = screen_w.max(1) as f32;
    let sh = screen_h.max(1) as f32;
    let scale = (sw / iw).max(sh / ih);
    let w = iw * scale;
    let h = ih * scale;
    let x = (sw - w) * 0.5;
    let y = (sh - h) * 0.5;
    (x, y, w, h)
}
