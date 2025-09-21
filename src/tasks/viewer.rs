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
        let avg_color = average_color(&src);

        if let MattingMode::Studio {
            bevel_width_px,
            highlight_strength,
            shadow_strength,
            bevel_angle_deg,
            linen_intensity,
            linen_scale_px,
            linen_rotation_deg,
            light_dir,
            shadow_radius_px,
            shadow_offset_px,
        } = &matting.style
        {
            let mut bevel_px = bevel_width_px.max(0.0);
            let margin_x = (canvas_w as f32 * margin).round();
            let margin_y = (canvas_h as f32 * margin).round();
            let inner_w = (canvas_w as f32 - 2.0 * margin_x).max(1.0);
            let inner_h = (canvas_h as f32 - 2.0 * margin_y).max(1.0);
            let max_bevel = 0.5 * inner_w.min(inner_h).max(0.0);
            if max_bevel <= 0.0 {
                bevel_px = 0.0;
            } else {
                bevel_px = bevel_px.min(max_bevel);
            }
            let photo_space_w = (canvas_w as f32 - 2.0 * (margin_x + bevel_px)).max(1.0);
            let photo_space_h = (canvas_h as f32 - 2.0 * (margin_y + bevel_px)).max(1.0);

            let iw = width.max(1) as f32;
            let ih = height.max(1) as f32;
            let mut scale = (photo_space_w / iw)
                .min(photo_space_h / ih)
                .min(max_upscale);
            if !scale.is_finite() || scale <= 0.0 {
                scale = 1.0;
            }
            let max_photo_w = photo_space_w.floor().max(1.0);
            let max_photo_h = photo_space_h.floor().max(1.0);
            let mut photo_w = (iw * scale).round().clamp(1.0, max_photo_w);
            let mut photo_h = (ih * scale).round().clamp(1.0, max_photo_h);
            photo_w = photo_w.clamp(1.0, canvas_w as f32);
            photo_h = photo_h.clamp(1.0, canvas_h as f32);
            let photo_w = photo_w as u32;
            let photo_h = photo_h as u32;
            let (offset_x, offset_y) = center_offset(photo_w, photo_h, canvas_w, canvas_h);

            let main_img: Cow<'_, RgbaImage> = if photo_w == width && photo_h == height {
                Cow::Borrowed(&src)
            } else {
                Cow::Owned(imageops::resize(
                    &src,
                    photo_w,
                    photo_h,
                    imageops::FilterType::Triangle,
                ))
            };

            let canvas = render_studio_mat(
                path.as_path(),
                width,
                height,
                canvas_w,
                canvas_h,
                offset_x,
                offset_y,
                photo_w,
                photo_h,
                main_img.as_ref(),
                avg_color,
                bevel_px,
                *bevel_angle_deg,
                *highlight_strength,
                *shadow_strength,
                *linen_intensity,
                *linen_scale_px,
                *linen_rotation_deg,
                *light_dir,
                *shadow_radius_px,
                *shadow_offset_px,
            );

            let canvas = ImagePlane {
                width: canvas_w,
                height: canvas_h,
                pixels: canvas.into_raw(),
            };

            return Some(MatResult { path, canvas });
        }

        let (final_w, final_h) =
            resize_to_fit_with_margin(canvas_w, canvas_h, width, height, margin, max_upscale);
        let (offset_x, offset_y) = center_offset(final_w, final_h, canvas_w, canvas_h);

        let main_img: Cow<'_, RgbaImage> = if final_w == width && final_h == height {
            Cow::Borrowed(&src)
        } else {
            Cow::Owned(imageops::resize(
                &src,
                final_w,
                final_h,
                imageops::FilterType::Triangle,
            ))
        };

        let mut background = match &matting.style {
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
                if *sigma > 0.0 {
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
                    let mut sigma_px = *sigma;
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

                    let mut blurred: RgbaImage = apply_blur(&sample, sigma_px, *backend);
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
            MattingMode::Studio { .. } => unreachable!(),
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

        imageops::overlay(
            &mut background,
            main_img.as_ref(),
            offset_x as i64,
            offset_y as i64,
        );

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
    photo_x: u32,
    photo_y: u32,
    photo_w: u32,
    photo_h: u32,
    photo: &RgbaImage,
    avg_color: [f32; 3],
    bevel_width_px: f32,
    bevel_angle_deg: f32,
    highlight_strength: f32,
    shadow_strength: f32,
    linen_intensity: f32,
    linen_scale_px: f32,
    linen_rotation_deg: f32,
    light_dir: [f32; 3],
    shadow_radius_px: f32,
    shadow_offset_px: f32,
) -> RgbaImage {
    let seed = studio_seed(
        path, src_w, src_h, canvas_w, canvas_h, photo_x, photo_y, photo_w, photo_h,
    );
    let coarse_seed = seed;
    let fine_seed = seed.rotate_left(17) ^ 0xa076_1d64_78bd_642f;
    let fiber_seed = seed.rotate_left(29) ^ 0xe703_7ed1_a0b4_28db;

    let mut bevel_px = bevel_width_px.max(0.0);
    let max_border = photo_x
        .min(photo_y)
        .min(canvas_w.saturating_sub(photo_x.saturating_add(photo_w)))
        .min(canvas_h.saturating_sub(photo_y.saturating_add(photo_h))) as f32;
    if bevel_px > 0.0 {
        bevel_px = bevel_px.min(max_border.max(0.0));
    } else {
        bevel_px = 0.0;
    }

    let visible_w = photo_w.max(1) as f32;
    let visible_h = photo_h.max(1) as f32;
    let window_x = photo_x as f32;
    let window_y = photo_y as f32;
    let window_max_x = window_x + visible_w;
    let window_max_y = window_y + visible_h;

    let outer_min_x = (window_x - bevel_px).max(0.0);
    let outer_min_y = (window_y - bevel_px).max(0.0);
    let outer_max_x = (window_max_x + bevel_px).min(canvas_w as f32);
    let outer_max_y = (window_max_y + bevel_px).min(canvas_h as f32);

    let luma = (avg_color[0] * 0.299 + avg_color[1] * 0.587 + avg_color[2] * 0.114).clamp(0.0, 1.0);
    let mut base = [0.0f32; 3];
    for (i, channel) in base.iter_mut().enumerate() {
        let softened = lerp(avg_color[i], luma, 0.25);
        *channel = lerp(softened, 0.92, 0.12).clamp(0.05, 0.97);
    }

    let linen_strength = linen_intensity.clamp(0.0, 1.0);
    let linen_scale = linen_scale_px.max(48.0);
    let linen_amp = 0.015 + linen_strength * 0.085;
    let rotation = linen_rotation_deg.to_radians();
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let cx = canvas_w as f32 * 0.5;
    let cy = canvas_h as f32 * 0.5;

    let mut light = normalize3(light_dir);
    if light[2].abs() <= f32::EPSILON {
        light[2] = 0.2;
        light = normalize3(light);
    }
    let light_xy = normalize2([light[0], light[1]]);

    let highlight_scale = highlight_strength.clamp(0.0, 1.0) * 0.22;
    let shadow_scale = shadow_strength.clamp(0.0, 1.0) * 0.22;
    let bevel_angle = bevel_angle_deg.to_radians().abs().max(1.0f32.to_radians());
    let bevel_xy = bevel_angle.cos().abs().max(0.01);
    let bevel_z = bevel_angle.sin().abs().max(0.01);

    let shadow_radius = shadow_radius_px.max(0.1);
    let shadow_offset = shadow_offset_px.max(0.0);
    let shadow_weight = shadow_strength.clamp(0.0, 1.0) * 0.35;

    let mut mat = RgbaImage::new(canvas_w, canvas_h);
    for (x, y, pixel) in mat.enumerate_pixels_mut() {
        let px = x as f32 + 0.5;
        let py = y as f32 + 0.5;

        let rx = (px - cx) * cos_r - (py - cy) * sin_r;
        let ry = (px - cx) * sin_r + (py - cy) * cos_r;
        let linen = if linen_strength > 0.0 {
            let u = rx / linen_scale;
            let v = ry / linen_scale;
            let jitter_u = fbm_noise(coarse_seed, v * 4.7, v * 3.1, 4) * 0.12 - 0.06;
            let jitter_v = fbm_noise(fine_seed, u * 4.1, u * 3.9, 4) * 0.12 - 0.06;

            let warp = (std::f32::consts::TAU * (u + jitter_u)).sin();
            let weft = (std::f32::consts::TAU * (v + jitter_v)).sin();
            let warp_detail = (std::f32::consts::TAU * (u * 2.0 + jitter_v * 0.5)).sin();
            let weft_detail = (std::f32::consts::TAU * (v * 2.0 + jitter_u * 0.5)).sin();
            let warp_band = warp.abs() - 0.63;
            let weft_band = weft.abs() - 0.63;
            let cross = warp * weft * 0.35;
            let detail = (warp_detail + weft_detail) * 0.18;
            let fiber = (fbm_noise(fiber_seed, u * 18.0, v * 18.0, 4) - 0.5) * 0.6;
            let weave = warp_band + weft_band + cross + detail + fiber;
            (1.0 + linen_amp * weave).clamp(1.0 - linen_amp * 2.2, 1.0 + linen_amp * 2.0)
        } else {
            1.0
        };

        let mut base_color = [0.0f32; 3];
        for c in 0..3 {
            base_color[c] = (base[c] * linen).clamp(0.0, 1.0);
        }

        let inside_window =
            px >= window_x && px <= window_max_x && py >= window_y && py <= window_max_y;

        if inside_window {
            let u = if visible_w <= f32::EPSILON {
                0.0
            } else {
                ((px - window_x) / visible_w).clamp(0.0, 1.0)
            };
            let v = if visible_h <= f32::EPSILON {
                0.0
            } else {
                ((py - window_y) / visible_h).clamp(0.0, 1.0)
            };
            let sample_x = (u * (photo_w.max(1) as f32 - 1.0)).clamp(0.0, photo_w as f32 - 1.0);
            let sample_y = (v * (photo_h.max(1) as f32 - 1.0)).clamp(0.0, photo_h as f32 - 1.0);
            let mut sample = sample_bilinear(photo, sample_x, sample_y);

            let dist_left = (px - window_x).max(0.0);
            let dist_right = (window_max_x - px).max(0.0);
            let dist_top = (py - window_y).max(0.0);
            let dist_bottom = (window_max_y - py).max(0.0);
            let (edge_normal, edge_distance) =
                if dist_left <= dist_right && dist_left <= dist_top && dist_left <= dist_bottom {
                    ([-1.0, 0.0], dist_left)
                } else if dist_right <= dist_top && dist_right <= dist_bottom {
                    ([1.0, 0.0], dist_right)
                } else if dist_top <= dist_bottom {
                    ([0.0, -1.0], dist_top)
                } else {
                    ([0.0, 1.0], dist_bottom)
                };

            let directional = if light_xy == [0.0, 0.0] {
                0.0
            } else {
                (edge_normal[0] * -light_xy[0] + edge_normal[1] * -light_xy[1]).max(0.0)
            };
            let shadow_dist = (edge_distance - shadow_offset).max(0.0);
            let falloff = 1.0 - smoothstep((shadow_dist / shadow_radius).clamp(0.0, 1.0));
            let shadow = (shadow_weight * directional * falloff).clamp(0.0, 0.6);
            for c in 0..3 {
                sample[c] = (sample[c] * (1.0 - shadow)).clamp(0.0, 1.0);
            }

            for c in 0..3 {
                pixel[c] = srgb_u8(sample[c]);
            }
            pixel[3] = 255;
            continue;
        }

        let in_outer =
            px >= outer_min_x && px <= outer_max_x && py >= outer_min_y && py <= outer_max_y;

        if in_outer && bevel_px > 0.0 {
            let dist_left = (window_x - px).max(0.0);
            let dist_right = (px - window_max_x).max(0.0);
            let dist_top = (window_y - py).max(0.0);
            let dist_bottom = (py - window_max_y).max(0.0);

            let mut edge_normal_xy = [-1.0, 0.0];
            let mut raw_distance = dist_left;
            if dist_right > raw_distance {
                edge_normal_xy = [1.0, 0.0];
                raw_distance = dist_right;
            }
            if dist_top > raw_distance {
                edge_normal_xy = [0.0, -1.0];
                raw_distance = dist_top;
            }
            if dist_bottom > raw_distance {
                edge_normal_xy = [0.0, 1.0];
                raw_distance = dist_bottom;
            }

            let edge_distance = raw_distance.min(bevel_px);
            let depth = if bevel_px <= f32::EPSILON {
                0.0
            } else {
                (edge_distance / bevel_px).clamp(0.0, 1.0)
            };

            let mut normal = [
                edge_normal_xy[0] * bevel_xy,
                edge_normal_xy[1] * bevel_xy,
                bevel_z,
            ];
            normal = normalize3(normal);
            let dot = normal[0] * light[0] + normal[1] * light[1] + normal[2] * light[2];
            let directional = if dot >= 0.0 {
                highlight_scale * dot
            } else {
                shadow_scale * dot
            };
            let gradient = lerp(1.1, 0.94, depth);
            let bevel_value = (0.9 + directional) * gradient;

            let inner_mix = (1.0 - depth).clamp(0.0, 1.0);
            let dist_outer = (px - outer_min_x)
                .min(outer_max_x - px)
                .min(py - outer_min_y)
                .min(outer_max_y - py)
                .max(0.0);
            let feather = smoothstep(dist_outer.clamp(0.0, 1.0));
            let mix = (inner_mix * feather).clamp(0.0, 1.0);

            for c in 0..3 {
                let value = lerp(base_color[c], bevel_value.clamp(0.0, 1.0), mix);
                pixel[c] = srgb_u8(value);
            }
            pixel[3] = 255;
        } else {
            for c in 0..3 {
                pixel[c] = srgb_u8(base_color[c]);
            }
            pixel[3] = 255;
        }
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

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= f32::EPSILON {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn normalize2(v: [f32; 2]) -> [f32; 2] {
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len <= f32::EPSILON {
        [0.0, 0.0]
    } else {
        [v[0] / len, v[1] / len]
    }
}

fn sample_bilinear(img: &RgbaImage, x: f32, y: f32) -> [f32; 3] {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return [0.0, 0.0, 0.0];
    }
    let max_x = (w - 1) as f32;
    let max_y = (h - 1) as f32;
    let xf = x.clamp(0.0, max_x);
    let yf = y.clamp(0.0, max_y);
    let x0 = xf.floor() as u32;
    let y0 = yf.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = xf - x0 as f32;
    let ty = yf - y0 as f32;

    let p00 = img.get_pixel(x0, y0);
    let p10 = img.get_pixel(x1, y0);
    let p01 = img.get_pixel(x0, y1);
    let p11 = img.get_pixel(x1, y1);

    let mut result = [0.0f32; 3];
    for c in 0..3 {
        let c00 = p00[c] as f32 / 255.0;
        let c10 = p10[c] as f32 / 255.0;
        let c01 = p01[c] as f32 / 255.0;
        let c11 = p11[c] as f32 / 255.0;
        let c0 = lerp(c00, c10, tx);
        let c1 = lerp(c01, c11, tx);
        result[c] = lerp(c0, c1, ty);
    }
    result
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
