use crate::config::{
    MattingConfig, MattingMode, MattingOptions, TransitionConfig, TransitionKind, TransitionMode,
    TransitionOptions,
};
use crate::events::{Displayed, PhotoLoaded, PreparedImageCpu};
use crate::processing::blur::apply_blur;
use crate::processing::color::average_color;
use crate::processing::layout::{center_offset, resize_to_cover};
use crate::tasks::greeting_screen::GreetingScreen;
use crossbeam_channel::{bounded, Receiver as CbReceiver, Sender as CbSender, TrySendError};
use image::{imageops, Rgba, RgbaImage};
use rand::Rng;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

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
    struct TransitionUniforms {
        screen_size: [f32; 2],
        progress: f32,
        kind: u32,
        current_dest: [f32; 4],
        next_dest: [f32; 4],
        params0: [f32; 4],
        params1: [f32; 4],
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
        blank_plane: TexturePlane,
        greeting: GreetingScreen,
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

    struct TransitionState {
        started_at: std::time::Instant,
        duration: std::time::Duration,
        kind: TransitionKind,
        variant: ActiveTransition,
    }

    enum ActiveTransition {
        Fade {
            through_black: bool,
        },
        Wipe {
            normal: [f32; 2],
            softness: f32,
        },
        Push {
            direction: [f32; 2],
        },
        EInk {
            flash_count: u32,
            reveal_portion: f32,
            stripe_count: u32,
            flash_color: [f32; 3],
            noise_seed: [f32; 2],
        },
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
            bevel_color,
            texture_strength,
            warp_period_px,
            weft_period_px,
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
                canvas_w,
                canvas_h,
                offset_x,
                offset_y,
                photo_w,
                photo_h,
                main_img.as_ref(),
                avg_color,
                bevel_px,
                *bevel_color,
                *texture_strength,
                *warp_period_px,
                *weft_period_px,
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
                max_sample_dimension,
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
                    let limit = max_sample_dimension
                        .filter(|v| *v > 0)
                        .unwrap_or({
                            #[cfg(target_arch = "aarch64")]
                            {
                                MattingMode::default_blur_max_sample_dimension()
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
                    match bg.canvas_for(*fit, canvas_w, canvas_h, max_dim) {
                        Ok(prepared) => prepared.as_ref().clone(),
                        Err(err) => {
                            warn!(
                                "failed to prepare fixed background from {}: {err}",
                                bg.path().display()
                            );
                            RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 255]))
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

    impl TransitionState {
        fn new(
            option: TransitionOptions,
            started_at: std::time::Instant,
            rng: &mut impl Rng,
        ) -> Self {
            let duration = option.duration();
            let kind = option.kind();
            let mode = option.mode().clone();
            let variant = match mode {
                TransitionMode::Fade(cfg) => ActiveTransition::Fade {
                    through_black: cfg.through_black,
                },
                TransitionMode::Wipe(cfg) => {
                    let angle = cfg.angles.pick_angle(rng);
                    let (sin, cos) = angle.to_radians().sin_cos();
                    let mut normal = [cos, sin];
                    let len = (normal[0] * normal[0] + normal[1] * normal[1]).sqrt();
                    if len > f32::EPSILON {
                        normal[0] /= len;
                        normal[1] /= len;
                    } else {
                        normal = [1.0, 0.0];
                    }
                    ActiveTransition::Wipe {
                        normal,
                        softness: cfg.softness,
                    }
                }
                TransitionMode::Push(cfg) => {
                    let angle = cfg.angles.pick_angle(rng);
                    let (sin, cos) = angle.to_radians().sin_cos();
                    let mut direction = [cos, sin];
                    let len = (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
                    if len > f32::EPSILON {
                        direction[0] /= len;
                        direction[1] /= len;
                    } else {
                        direction = [1.0, 0.0];
                    }
                    ActiveTransition::Push { direction }
                }
                TransitionMode::EInk(cfg) => ActiveTransition::EInk {
                    flash_count: cfg.flash_count,
                    reveal_portion: cfg.reveal_portion.clamp(0.05, 0.95),
                    stripe_count: cfg.stripe_count.max(1),
                    flash_color: cfg
                        .flash_color
                        .map(|channel| (channel as f32 / 255.0).clamp(0.0, 1.0)),
                    noise_seed: [rng.random(), rng.random()],
                },
            };
            Self {
                started_at,
                duration,
                kind,
                variant,
            }
        }

        fn progress(&self) -> f32 {
            let elapsed = self.started_at.elapsed();
            let total = self.duration.as_secs_f32().max(1e-3);
            (elapsed.as_secs_f32() / total).clamp(0.0, 1.0)
        }

        fn is_complete(&self) -> bool {
            self.started_at.elapsed() >= self.duration
        }
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
        transition_state: Option<TransitionState>,
        displayed_at: Option<std::time::Instant>,
        dwell_ms: u64,
        greeting_duration: Duration,
        greeting_deadline: Option<Instant>,
        pending: VecDeque<ImgTex>,
        preload_count: usize,
        oversample: f32,
        matting: MattingConfig,
        transition_cfg: TransitionConfig,
        mat_pipeline: MattingPipeline,
        mat_inflight: usize,
        ready_results: VecDeque<MatResult>,
        deferred_images: VecDeque<PreparedImageCpu>,
        clear_color: wgpu::Color,
        rng: rand::rngs::ThreadRng,
        full_config: crate::config::Configuration,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.pending.clear();
            self.ready_results.clear();
            self.deferred_images.clear();
            self.current = None;
            self.next = None;
            self.transition_state = None;
            self.displayed_at = None;
            self.greeting_deadline = Some(Instant::now() + self.greeting_duration);
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
                size: std::mem::size_of::<TransitionUniforms>() as u64,
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
                bind_group_layouts: &[&uniform_layout, &img_bind_layout, &img_bind_layout],
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
            let make_plane = |label: &str, width: u32, height: u32, data: &[u8]| -> TexturePlane {
                let w = width.max(1);
                let h = height.max(1);
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * w),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
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
                TexturePlane { bind, w, h }
            };

            let blank_plane = make_plane("blank-texture", 1, 1, &[0, 0, 0, 255]);

            let mut greeting = GreetingScreen::new(&device, &queue, format, &self.full_config);
            greeting.resize(size, window.scale_factor());

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
                blank_plane,
                greeting,
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
                    gpu.greeting.resize(new_size, window.scale_factor());
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
                    gpu.greeting.resize(size, window.scale_factor());
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

                    if self.current.is_none() && self.transition_state.is_none() {
                        gpu.greeting.render(&mut encoder, &view);
                        gpu.queue.submit(Some(encoder.finish()));
                        frame.present();
                        return;
                    }
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
                    let screen_w = gpu.config.width as f32;
                    let screen_h = gpu.config.height as f32;
                    let mut uniforms = TransitionUniforms {
                        screen_size: [screen_w, screen_h],
                        progress: 0.0,
                        kind: 0,
                        current_dest: [0.0; 4],
                        next_dest: [0.0; 4],
                        params0: [0.0; 4],
                        params1: [0.0; 4],
                    };
                    let mut current_bind = &gpu.blank_plane.bind;
                    let mut next_bind = &gpu.blank_plane.bind;
                    let mut have_draw = false;

                    if let Some(state) = &self.transition_state {
                        if let Some(cur) = &self.current {
                            let rect = compute_cover_rect(
                                cur.plane.w,
                                cur.plane.h,
                                gpu.config.width,
                                gpu.config.height,
                            );
                            uniforms.current_dest = rect_to_uniform(rect);
                            current_bind = &cur.plane.bind;
                            have_draw = true;
                        }
                        if let Some(next) = &self.next {
                            let rect = compute_cover_rect(
                                next.plane.w,
                                next.plane.h,
                                gpu.config.width,
                                gpu.config.height,
                            );
                            uniforms.next_dest = rect_to_uniform(rect);
                            next_bind = &next.plane.bind;
                            have_draw = true;
                        }
                        let mut progress = state.progress();
                        progress = progress * progress * (3.0 - 2.0 * progress);
                        uniforms.progress = progress;
                        uniforms.kind = state.kind.as_index();
                        match &state.variant {
                            ActiveTransition::Fade { through_black } => {
                                uniforms.params0[0] = if *through_black { 1.0 } else { 0.0 };
                            }
                            ActiveTransition::Wipe { normal, softness } => {
                                let (min_proj, inv_span) =
                                    compute_wipe_span(*normal, screen_w, screen_h);
                                uniforms.params0 = [normal[0], normal[1], min_proj, inv_span];
                                uniforms.params1[0] = *softness;
                            }
                            ActiveTransition::Push { direction } => {
                                let diag = (screen_w * screen_w + screen_h * screen_h).sqrt();
                                uniforms.params0[0] = direction[0] * diag;
                                uniforms.params0[1] = direction[1] * diag;
                            }
                            ActiveTransition::EInk {
                                flash_count,
                                reveal_portion,
                                stripe_count,
                                flash_color,
                                noise_seed,
                            } => {
                                uniforms.params0[0] = (*flash_count).min(6) as f32;
                                uniforms.params0[1] = *reveal_portion;
                                uniforms.params0[2] = (*stripe_count).max(1) as f32;
                                uniforms.params0[3] = noise_seed[0];
                                uniforms.params1[0] = noise_seed[1];
                                uniforms.params1[1] = flash_color[0].clamp(0.0, 1.0);
                                uniforms.params1[2] = flash_color[1].clamp(0.0, 1.0);
                                uniforms.params1[3] = flash_color[2].clamp(0.0, 1.0);
                            }
                        }
                    } else if let Some(cur) = &self.current {
                        let rect = compute_cover_rect(
                            cur.plane.w,
                            cur.plane.h,
                            gpu.config.width,
                            gpu.config.height,
                        );
                        uniforms.current_dest = rect_to_uniform(rect);
                        current_bind = &cur.plane.bind;
                        have_draw = true;
                    }
                    if have_draw {
                        gpu.queue
                            .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
                        rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
                        rpass.set_bind_group(1, current_bind, &[]);
                        rpass.set_bind_group(2, next_bind, &[]);
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
                let mut rng = rand::rng();
                let matting = self.matting.choose_option(&mut rng);
                let params = MatParams {
                    screen_w: gpu.config.width.max(1),
                    screen_h: gpu.config.height.max(1),
                    oversample: self.oversample,
                    max_dim: gpu.limits.max_texture_dimension_2d,
                    matting,
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
            if self.current.is_none() && self.transition_state.is_none() {
                let greeting_finished = self
                    .greeting_deadline
                    .map(|deadline| Instant::now() >= deadline)
                    .unwrap_or(true);
                if greeting_finished {
                    if let Some(first) = self.pending.pop_front() {
                        info!("first_image path={}", first.path.display());
                        self.current = Some(first);
                        self.greeting_deadline = None;
                        self.displayed_at = Some(std::time::Instant::now());
                        if let Some(cur) = &self.current {
                            let _ = self
                                .to_manager_displayed
                                .try_send(Displayed(cur.path.clone()));
                        }
                    }
                }
            }
            if self
                .transition_state
                .as_ref()
                .is_some_and(TransitionState::is_complete)
            {
                let state = self
                    .transition_state
                    .take()
                    .expect("transition state should exist when complete");
                if let Some(next) = self.next.take() {
                    let path = next.path.clone();
                    self.current = Some(next);
                    self.displayed_at = Some(std::time::Instant::now());
                    info!(
                        "transition_end kind={} path={} queue_depth={}",
                        state.kind,
                        path.display(),
                        self.pending.len()
                    );
                    let _ = self.to_manager_displayed.try_send(Displayed(path));
                }
            }
            if self.transition_state.is_none() {
                if let Some(shown_at) = self.displayed_at {
                    if shown_at.elapsed() >= std::time::Duration::from_millis(self.dwell_ms) {
                        if self.next.is_none() {
                            if let Some(stage) = self.pending.pop_front() {
                                info!(
                                    "transition_stage path={} queue_depth={}",
                                    stage.path.display(),
                                    self.pending.len()
                                );
                                self.next = Some(stage);
                            }
                        }
                        if self.next.is_some() && self.current.is_some() {
                            let option = self.transition_cfg.choose_option(&mut self.rng);
                            let kind = option.kind();
                            let state = TransitionState::new(
                                option,
                                std::time::Instant::now(),
                                &mut self.rng,
                            );
                            if let Some(next) = &self.next {
                                info!(
                                    "transition_start kind={} path={} queue_depth={}",
                                    kind,
                                    next.path.display(),
                                    self.pending.len()
                                );
                            }
                            self.transition_state = Some(state);
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
    let clear_color = cfg
        .matting
        .primary_option()
        .and_then(MattingOptions::fixed_color)
        .map(|color| wgpu::Color {
            r: (color[0] as f64) / 255.0,
            g: (color[1] as f64) / 255.0,
            b: (color[2] as f64) / 255.0,
            a: 1.0,
        })
        .unwrap_or(wgpu::Color::BLACK);
    let mut app = App {
        from_loader,
        to_manager_displayed,
        cancel,
        window: None,
        gpu: None,
        current: None,
        next: None,
        transition_state: None,
        displayed_at: None,
        dwell_ms: cfg.dwell_ms,
        greeting_duration: cfg.greeting_screen.effective_duration(),
        greeting_deadline: None,
        pending: VecDeque::new(),
        preload_count: cfg.viewer_preload_count,
        oversample: cfg.oversample,
        matting: cfg.matting.clone(),
        transition_cfg: cfg.transition.clone(),
        mat_pipeline,
        mat_inflight: 0,
        ready_results: VecDeque::new(),
        deferred_images: VecDeque::new(),
        clear_color,
        rng: rand::rng(),
        full_config: cfg.clone(),
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

#[allow(clippy::too_many_arguments)]
// The mat renderer needs the full geometry and color context to avoid heap allocations.
fn render_studio_mat(
    canvas_w: u32,
    canvas_h: u32,
    photo_x: u32,
    photo_y: u32,
    photo_w: u32,
    photo_h: u32,
    photo: &RgbaImage,
    mat_color: [f32; 3],
    bevel_width_px: f32,
    bevel_color: [u8; 3],
    texture_strength: f32,
    warp_period_px: f32,
    weft_period_px: f32,
) -> RgbaImage {
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

    let window_x = photo_x as f32;
    let window_y = photo_y as f32;
    let window_max_x = window_x + photo_w.max(1) as f32;
    let window_max_y = window_y + photo_h.max(1) as f32;

    let bevel_rgb_f32 = [
        bevel_color[0] as f32 / 255.0,
        bevel_color[1] as f32 / 255.0,
        bevel_color[2] as f32 / 255.0,
    ];
    let light_dir = normalize3([-0.55, -0.65, 0.52]);
    let ambient = 0.88;
    let diffuse = 0.18;
    let texture_strength = texture_strength.clamp(0.0, 2.0);
    let warp_period = warp_period_px.max(0.5);
    let weft_period = weft_period_px.max(0.5);

    let mut mat = RgbaImage::new(canvas_w, canvas_h);
    for (x, y, pixel) in mat.enumerate_pixels_mut() {
        let px = x as f32 + 0.5;
        let py = y as f32 + 0.5;

        let inside_window =
            px >= window_x && px < window_max_x && py >= window_y && py < window_max_y;

        if inside_window {
            let u = if photo_w == 0 {
                0.0
            } else {
                ((px - window_x) / photo_w as f32).clamp(0.0, 1.0)
            };
            let v = if photo_h == 0 {
                0.0
            } else {
                ((py - window_y) / photo_h as f32).clamp(0.0, 1.0)
            };
            let sample_x = (u * (photo_w.max(1) as f32 - 1.0)).clamp(0.0, photo_w as f32 - 1.0);
            let sample_y = (v * (photo_h.max(1) as f32 - 1.0)).clamp(0.0, photo_h as f32 - 1.0);
            let sample = sample_bilinear(photo, sample_x, sample_y);

            for c in 0..3 {
                pixel[c] = srgb_u8(sample[c]);
            }
            pixel[3] = 255;
            continue;
        }

        if bevel_px > 0.0 {
            let dx = if px < window_x {
                window_x - px
            } else if px >= window_max_x {
                px - window_max_x
            } else {
                0.0
            };
            let dy = if py < window_y {
                window_y - py
            } else if py >= window_max_y {
                py - window_max_y
            } else {
                0.0
            };

            if dx < bevel_px && dy < bevel_px {
                let max_offset = dx.max(dy).clamp(0.0, bevel_px);
                let depth = if bevel_px <= f32::EPSILON {
                    0.0
                } else {
                    (1.0 - max_offset / bevel_px).clamp(0.0, 1.0)
                };

                let nearest_x = px.clamp(window_x, window_max_x);
                let nearest_y = py.clamp(window_y, window_max_y);
                let mut dir = [nearest_x - px, nearest_y - py];
                let dir_len_sq = dir[0] * dir[0] + dir[1] * dir[1];
                if dir_len_sq > 1e-6 {
                    let inv_len = dir_len_sq.sqrt().recip();
                    dir[0] *= inv_len;
                    dir[1] *= inv_len;
                } else if dx > dy {
                    dir = [if px < window_x { 1.0 } else { -1.0 }, 0.0];
                } else {
                    dir = [0.0, if py < window_y { 1.0 } else { -1.0 }];
                }

                let mut normal = [dir[0], dir[1], 1.0];
                normal = normalize3(normal);
                let mut shade = ambient + diffuse * dot3(normal, light_dir).max(0.0);
                shade += 0.1 * depth.powf(2.0);
                shade = shade.clamp(0.82, 1.08);

                let mat_mix = (1.0 - depth).powf(3.0) * 0.35;
                let mat_mix = mat_mix.clamp(0.0, 1.0);

                let mut color = [0u8; 3];
                for c in 0..3 {
                    let base = lerp(bevel_rgb_f32[c], mat_color[c], mat_mix);
                    let shaded = (base * shade).clamp(0.0, 1.0);
                    color[c] = srgb_u8(shaded);
                }

                pixel[0] = color[0];
                pixel[1] = color[1];
                pixel[2] = color[2];
                pixel[3] = 255;
                continue;
            }
        }

        let warp_noise = (weave_grain(x, y) - 0.5) * 0.65;
        let weft_noise = (weave_grain(x.wrapping_add(17), y.wrapping_add(113)) - 0.5) * 0.65;
        let warp_phase = ((px + warp_noise) / warp_period).fract();
        let weft_phase = ((py + weft_noise) / weft_period).fract();
        let warp_profile = weave_thread_profile(warp_phase);
        let weft_profile = weave_thread_profile(weft_phase);
        let warp_centered = warp_profile - 0.5;
        let weft_centered = weft_profile - 0.5;
        let cross_highlight = warp_profile * weft_profile - 0.25;
        let thread_mix = (warp_centered * 0.08 - weft_centered * 0.06 + cross_highlight * 0.12)
            * texture_strength;
        let grain_strength = texture_strength.min(1.0);
        let grain =
            (weave_grain(x.wrapping_add(137), y.wrapping_add(197)) - 0.5) * 0.025 * grain_strength;
        let envelope = 0.1 * texture_strength.min(1.2);
        let shade = (1.0 + thread_mix + grain).clamp(1.0 - envelope, 1.0 + envelope);

        for c in 0..3 {
            let tinted = (mat_color[c] * shade).clamp(0.0, 1.0);
            pixel[c] = srgb_u8(tinted);
        }
        pixel[3] = 255;
    }

    mat
}

fn srgb_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(mut v: [f32; 3]) -> [f32; 3] {
    let len_sq = dot3(v, v);
    if len_sq > 1e-6 {
        let inv_len = len_sq.sqrt().recip();
        v[0] *= inv_len;
        v[1] *= inv_len;
        v[2] *= inv_len;
        v
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn weave_thread_profile(phase: f32) -> f32 {
    let dist = (phase - 0.5).abs() * 2.0;
    let base = (1.0 - dist).clamp(0.0, 1.0);
    base * base * (3.0 - 2.0 * base)
}

fn weave_grain(x: u32, y: u32) -> f32 {
    let mut hash = x.wrapping_mul(0x045d_9f3b) ^ y.wrapping_mul(0x27d4_eb2d);
    hash ^= hash.rotate_left(13);
    hash = hash.wrapping_mul(0x1656_67b1);
    ((hash >> 8) & 0xffff) as f32 / 65535.0
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

fn rect_to_uniform(rect: (f32, f32, f32, f32)) -> [f32; 4] {
    [rect.0, rect.1, rect.2, rect.3]
}

fn compute_wipe_span(normal: [f32; 2], screen_w: f32, screen_h: f32) -> (f32, f32) {
    let corners = [
        [0.0, 0.0],
        [screen_w, 0.0],
        [0.0, screen_h],
        [screen_w, screen_h],
    ];
    let mut min_proj = f32::MAX;
    let mut max_proj = f32::MIN;
    for corner in corners {
        let proj = normal[0] * corner[0] + normal[1] * corner[1];
        min_proj = min_proj.min(proj);
        max_proj = max_proj.max(proj);
    }
    let span = (max_proj - min_proj).abs().max(1e-3);
    (min_proj, 1.0 / span)
}
