use anyhow::{Context, Result};
use crossbeam_channel as xchan;
use std::{path::PathBuf, sync::Arc, time::Instant};
use tracing::info;
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

use crate::render::loader::{LoaderMsg, PreparedImage, spawn_loader};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
}

const QUAD: [Vertex; 4] = [
    //   NDC pos         UV
    Vertex {
        pos: [-1.0, -1.0],
        uv: [0.0, 1.0],
    }, // bottom-left
    Vertex {
        pos: [1.0, -1.0],
        uv: [1.0, 1.0],
    }, // bottom-right
    Vertex {
        pos: [-1.0, 1.0],
        uv: [0.0, 0.0],
    }, // top-left
    Vertex {
        pos: [1.0, 1.0],
        uv: [1.0, 0.0],
    }, // top-right
];

/// Run the slideshow with a per-image delay in milliseconds.
///
/// # Errors
/// Returns an error if the rendering backend fails to initialize or submit work.
pub fn run_slideshow(paths: Vec<PathBuf>, delay_ms: u64) -> Result<()> {
    let delay = std::time::Duration::from_millis(delay_ms.max(1));
    info!(count = paths.len(), "starting slideshow");
    let event_loop = EventLoop::new()?;
    let mut app = App::new(paths, delay);
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct Tex {
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

struct Gpu {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    _adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    vbuf: wgpu::Buffer,

    // uniforms (32 bytes each to match WGSL)
    params_a: wgpu::Buffer, // [sx,sy,0,0] + pad vec4
    params_b: wgpu::Buffer, // [sx,sy,0,0] + pad vec4
    fade_buf: wgpu::Buffer, // [alpha,0,0,0] + pad vec4
    scale_a: [f32; 4],
    scale_b: [f32; 4],
    alpha: f32,

    // textures
    tex_a: Tex,
    tex_b: Tex,
    sampler: wgpu::Sampler,
}

struct App {
    // file list
    list: Vec<PathBuf>,
    cur_idx: usize,
    next_idx: usize,

    // timing
    last_switch: Instant,
    switch_interval: std::time::Duration,
    fade_start: Instant,
    fade_dur: std::time::Duration,
    fading: bool,

    // window/gpu
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,

    // decode pipeline
    tx_req: xchan::Sender<LoaderMsg>,
    rx_res: xchan::Receiver<PreparedImage>,
}

impl App {
    fn new(paths: Vec<PathBuf>, delay: std::time::Duration) -> Self {
        // background loader will be started after window init
        let (_tx_dummy, rx_res) = xchan::unbounded::<PreparedImage>();

        Self {
            list: paths,
            cur_idx: 0,
            next_idx: 1,

            last_switch: Instant::now(),
            switch_interval: delay,

            fade_start: Instant::now(),
            fade_dur: delay,
            fading: false,

            window: None,
            gpu: None,

            tx_req: xchan::unbounded::<LoaderMsg>().0,
            rx_res,
        }
    }

    fn current_path(&self) -> Option<&PathBuf> {
        self.list.get(self.cur_idx)
    }
    fn next_path(&self) -> Option<&PathBuf> {
        self.list.get(self.next_idx)
    }

    const fn advance_indices(&mut self) {
        if self.list.is_empty() {
            return;
        }
        self.cur_idx = self.next_idx;
        self.next_idx = (self.cur_idx + 1) % self.list.len();
    }
}

impl ApplicationHandler for App {
    #[allow(
        clippy::too_many_lines,
        clippy::collapsible_if,
        clippy::collapsible_match
    )]
    #[allow(
        clippy::too_many_lines,
        clippy::collapsible_if,
        clippy::collapsible_match
    )]
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // ----- window -----
        let attrs = WindowAttributes::default().with_title("photo viewer");
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let monitor = window.current_monitor();
        window.set_fullscreen(Some(Fullscreen::Borderless(monitor)));
        info!("window fullscreen initialized");
        window.set_cursor_visible(false);
        self.window = Some(window.clone());

        // ----- Start request-driven loader -----
        let PhysicalSize {
            width: win_w,
            height: win_h,
        } = window.inner_size();
        let (tx_req, rx_req) = xchan::unbounded::<LoaderMsg>();
        let (tx_res, rx_res) = xchan::unbounded::<PreparedImage>();
        spawn_loader(rx_req, tx_res);
        self.tx_req = tx_req;
        self.rx_res = rx_res;
        // queue current and next
        if let Some(p) = self.current_path() {
            let _ = self
                .tx_req
                .send(LoaderMsg::Decode(p.clone(), (win_w.max(1), win_h.max(1))));
        }
        if let Some(p) = self.next_path() {
            let _ = self
                .tx_req
                .send(LoaderMsg::Decode(p.clone(), (win_w.max(1), win_h.max(1))));
        }

        // ----- GPU init -----
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let gpu_init = async move {
            // adapter/device
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .context("no compatible GPU adapter found")?;

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("device"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                    },
                    None,
                )
                .await?;

            // surface configuration
            let caps = surface.get_capabilities(&adapter);
            let format = caps
                .formats
                .iter()
                .copied()
                .find(wgpu::TextureFormat::is_srgb)
                .unwrap_or(caps.formats[0]);
            let PhysicalSize { width, height } = window.inner_size();
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: width.max(1),
                height: height.max(1),
                present_mode: wgpu::PresentMode::AutoVsync,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
                desired_maximum_frame_latency: 1,
            };
            surface.configure(&device, &config);

            // texture helpers
            let make_tex = |device: &wgpu::Device,
                            queue: &wgpu::Queue,
                            w: u32,
                            h: u32,
                            pixels: &[u8]|
             -> Tex {
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("photo"),
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
                    tex.as_image_copy(),
                    pixels,
                    wgpu::ImageDataLayout {
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
                Tex {
                    view: tex.create_view(&wgpu::TextureViewDescriptor::default()),
                    w,
                    h,
                }
            };

            // A = black until first prepared image arrives; B = placeholder black
            let tex_a = make_tex(&device, &queue, 1, 1, &[0, 0, 0, 255]);
            let tex_b = make_tex(&device, &queue, 1, 1, &[0, 0, 0, 255]);

            // sampler
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });

            // uniforms (32 bytes each)
            let params_a = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("params_a"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let params_b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("params_b"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let fade_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fade"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            // quad vertex buffer
            let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("quad"),
                contents: bytemuck::cast_slice(&QUAD),
                usage: wgpu::BufferUsages::VERTEX,
            });

            // shader & pipeline
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/photo.wgsl").into()),
            });

            let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bind_layout"),
                entries: &[
                    // tex A, samp A
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
                    // tex B, samp B
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    // Params A
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Params B
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Fade
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bind_group"),
                layout: &bind_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&tex_a.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&tex_b.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: params_a.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: params_b.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: fade_buf.as_entire_binding(),
                    },
                ],
            });

            let vlayout = wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
            };

            let pip_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pipe_layout"),
                bind_group_layouts: &[&bind_layout],
                push_constant_ranges: &[],
            });

            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pipeline"),
                layout: Some(&pip_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[vlayout],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    strip_index_format: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            // initial uniform data
            let write32 = |buf: &wgpu::Buffer, data4: [f32; 4]| {
                let mut block = [0f32; 8]; // 8 * 4 = 32 bytes
                block[0..4].copy_from_slice(&data4);
                queue.write_buffer(buf, 0, bytemuck::bytes_of(&block));
            };

            let scale_a = compute_uv_scale(config.width, config.height, tex_a.w, tex_a.h);
            let scale_b = compute_uv_scale(config.width, config.height, tex_b.w, tex_b.h);
            write32(&params_a, scale_a);
            write32(&params_b, scale_b);
            write32(&fade_buf, [0.0, 0.0, 0.0, 0.0]); // alpha = 0

            Ok::<Gpu, anyhow::Error>(Gpu {
                _instance: instance,
                surface,
                _adapter: adapter,
                device,
                queue,
                config,
                pipeline,
                bind_layout,
                bind_group,
                vbuf,
                params_a,
                params_b,
                fade_buf,
                scale_a,
                scale_b,
                alpha: 0.0,
                tex_a,
                tex_b,
                sampler,
            })
        };

        self.gpu = Some(pollster::block_on(gpu_init).expect("GPU init"));

        // slideshow timers
        self.last_switch = Instant::now();
        self.fade_start = Instant::now();
        self.fading = false;
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(win) = &self.window else { return };
        if win.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => std::process::exit(0),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Released {
                    use winit::keyboard::{KeyCode, PhysicalKey};
                    if let PhysicalKey::Code(KeyCode::Escape | KeyCode::KeyQ) = event.physical_key {
                        std::process::exit(0)
                    }
                }
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                if let Some(gpu) = &mut self.gpu
                    && width > 0
                    && height > 0
                {
                    gpu.config.width = width;
                    gpu.config.height = height;
                    gpu.surface.configure(&gpu.device, &gpu.config);

                    gpu.scale_a = compute_uv_scale(width, height, gpu.tex_a.w, gpu.tex_a.h);
                    gpu.scale_b = compute_uv_scale(width, height, gpu.tex_b.w, gpu.tex_b.h);
                    let mut block = [0f32; 8];
                    block[0..4].copy_from_slice(&gpu.scale_a);
                    gpu.queue
                        .write_buffer(&gpu.params_a, 0, bytemuck::bytes_of(&block));
                    block = [0f32; 8];
                    block[0..4].copy_from_slice(&gpu.scale_b);
                    gpu.queue
                        .write_buffer(&gpu.params_b, 0, bytemuck::bytes_of(&block));
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        // receive decoded images (non-blocking)
        if let Some(gpu) = &mut self.gpu {
            while let Ok(decoded) = self.rx_res.try_recv() {
                let new_b = upload_texture(
                    &gpu.device,
                    &gpu.queue,
                    &decoded.pixels,
                    decoded.size.0,
                    decoded.size.1,
                );
                gpu.tex_b = new_b;
                gpu.scale_b = compute_uv_scale(
                    gpu.config.width,
                    gpu.config.height,
                    gpu.tex_b.w,
                    gpu.tex_b.h,
                );
                // write params_b (32 bytes)
                let mut block = [0f32; 8];
                block[0..4].copy_from_slice(&gpu.scale_b);
                gpu.queue
                    .write_buffer(&gpu.params_b, 0, bytemuck::bytes_of(&block));
            }

            // if B changed, rebuild bind group so it points at the new texture view
            {
                rebuild_bind_group(gpu);
            }

            // start a fade every second (if not already fading and we have a decoded B)
            let now = Instant::now();
            if !self.fading
                && ((gpu.tex_a.w == 1 && gpu.tex_a.h == 1 && gpu.tex_b.w > 1 && gpu.tex_b.h > 1)
                    || (now.duration_since(self.last_switch) >= self.switch_interval
                        && gpu.tex_b.w > 1
                        && gpu.tex_b.h > 1))
            {
                self.fading = true;
                self.fade_start = now;
                gpu.alpha = 0.0;
                let mut fb = [0f32; 8];
                fb[0] = gpu.alpha;
                gpu.queue
                    .write_buffer(&gpu.fade_buf, 0, bytemuck::bytes_of(&fb));
            }

            // update fade alpha
            if self.fading {
                let t = (now - self.fade_start).as_secs_f32() / self.fade_dur.as_secs_f32();
                let a = t.clamp(0.0, 1.0);
                if (a - gpu.alpha).abs() > 1e-4 {
                    gpu.alpha = a;
                    let mut fb = [0f32; 8];
                    fb[0] = gpu.alpha;
                    gpu.queue
                        .write_buffer(&gpu.fade_buf, 0, bytemuck::bytes_of(&fb));
                }
                if a >= 1.0 {
                    // commit B -> A, reset B to placeholder, rebuild bind group
                    gpu.tex_a = std::mem::replace(
                        &mut gpu.tex_b,
                        upload_texture(&gpu.device, &gpu.queue, &[0, 0, 0, 255], 1, 1),
                    );
                    gpu.scale_a = gpu.scale_b;
                    let mut block = [0f32; 8];
                    block[0..4].copy_from_slice(&gpu.scale_a);
                    gpu.queue
                        .write_buffer(&gpu.params_a, 0, bytemuck::bytes_of(&block));

                    // new B scale (for placeholder)
                    gpu.scale_b = compute_uv_scale(
                        gpu.config.width,
                        gpu.config.height,
                        gpu.tex_b.w,
                        gpu.tex_b.h,
                    );
                    let mut block_b = [0f32; 8];
                    block_b[0..4].copy_from_slice(&gpu.scale_b);
                    gpu.queue
                        .write_buffer(&gpu.params_b, 0, bytemuck::bytes_of(&block_b));

                    // reset fade to 0
                    gpu.alpha = 0.0;
                    let mut fb = [0f32; 8];
                    fb[0] = 0.0;
                    gpu.queue
                        .write_buffer(&gpu.fade_buf, 0, bytemuck::bytes_of(&fb));

                    rebuild_bind_group(gpu);
                    self.fading = false;
                    self.last_switch = now;

                    // advance pipeline
                    self.advance_indices();
                    if let Some(p) = self.next_path() {
                        let PhysicalSize {
                            width: win_w,
                            height: win_h,
                        } = self.window.as_ref().unwrap().inner_size();
                        let _ = self
                            .tx_req
                            .send(LoaderMsg::Decode(p.clone(), (win_w.max(1), win_h.max(1))));
                    }
                    // (loader.rs handles queueing); no-op
                }
            }

            // drive redraw
            if let Some(win) = &self.window {
                win.request_redraw();
            }
        }
    }
}

impl App {
    fn draw(&self) {
        let Some(gpu) = &self.gpu else { return };
        let Ok(frame) = gpu.surface.get_current_texture() else {
            return;
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rpass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rpass.set_pipeline(&gpu.pipeline);
            rpass.set_bind_group(0, &gpu.bind_group, &[]);
            rpass.set_vertex_buffer(0, gpu.vbuf.slice(..));
            rpass.draw(0..4, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
        frame.present();
    }
}

fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pixels: &[u8],
    w: u32,
    h: u32,
) -> Tex {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("photo"),
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
        tex.as_image_copy(),
        pixels,
        wgpu::ImageDataLayout {
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
    Tex {
        view: tex.create_view(&wgpu::TextureViewDescriptor::default()),
        w,
        h,
    }
}

fn rebuild_bind_group(gpu: &mut Gpu) {
    gpu.bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bind_group"),
        layout: &gpu.bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&gpu.tex_a.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&gpu.tex_b.view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: gpu.params_a.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: gpu.params_b.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: gpu.fade_buf.as_entire_binding(),
            },
        ],
    });
}

#[allow(clippy::cast_precision_loss)]
fn compute_uv_scale(win_w: u32, win_h: u32, img_w: u32, img_h: u32) -> [f32; 4] {
    let ww = win_w as f32;
    let wh = win_h as f32;
    let iw = img_w as f32;
    let ih = img_h as f32;

    if ww == 0.0 || wh == 0.0 || iw == 0.0 || ih == 0.0 {
        return [1.0, 1.0, 0.0, 0.0];
    }

    let win_ar = ww / wh;
    let img_ar = iw / ih;

    if img_ar > win_ar {
        // Image is wider than window: we need to scale UV Y up so 0..1 area shrinks vertically
        [1.0, img_ar / win_ar, 0.0, 0.0]
    } else {
        // Image is taller than window: scale UV X up so 0..1 area shrinks horizontally
        [win_ar / img_ar, 1.0, 0.0, 0.0]
    }
}
