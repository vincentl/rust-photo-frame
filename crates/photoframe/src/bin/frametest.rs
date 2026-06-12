//! Minimal fullscreen frame-rate probe for the photo frame's display stack.
//!
//! Renders the simplest possible workloads at native resolution and reports
//! frame cadence once per second, isolating presentation/pacing limits from
//! application complexity. Run it inside the kiosk session (same Wayland
//! socket and user as the real app):
//!
//! ```text
//! sudo -u kiosk env XDG_RUNTIME_DIR=/run/user/$(id -u kiosk) WAYLAND_DISPLAY=wayland-1 \
//!   /opt/photoframe/bin/frametest [mode] [present] [latency]
//! ```
//!
//! * `mode`: `solid` (clear only) | `tex` (one 4K texture) | `fade`
//!   (two 4K textures mixed per pixel — equivalent to the app's fade) —
//!   default `fade`
//! * `present`: `fifo` | `mailbox` | `immediate` — default `fifo`
//! * `latency`: desired max frame latency, default `2`
//!
//! Honors `WGPU_BACKEND=gl|vulkan` like the main binary.

use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Fullscreen, Window, WindowId};

const SHADER: &str = r#"
struct Params {
  t: f32,
  mode: u32,
  _pad0: f32,
  _pad1: f32,
};
@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VSOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
  var out: VSOut;
  let p = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
  out.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
  out.uv = vec2<f32>(p.x, 1.0 - p.y);
  return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
  if (P.mode == 0u) {
    // solid: animated flat color, no memory traffic
    return vec4<f32>(0.5 + 0.5 * sin(P.t), 0.2, 0.5 + 0.5 * cos(P.t), 1.0);
  }
  let a = textureSample(tex_a, samp, in.uv);
  if (P.mode == 1u) {
    return vec4<f32>(a.rgb, 1.0);
  }
  // fade: two-texture mix, the app's cheapest real transition
  let b = textureSample(tex_b, samp, in.uv);
  let m = 0.5 + 0.5 * sin(P.t);
  return vec4<f32>(mix(a.rgb, b.rgb, m), 1.0);
}
"#;

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    params_buf: wgpu::Buffer,
}

struct App {
    mode: u32,
    present: wgpu::PresentMode,
    latency: u32,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    started: Instant,
    window_start: Instant,
    frames: u32,
    worst_ms: f32,
    best_ms: f32,
    last_frame: Instant,
}

fn checker_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    phase: u32,
) -> wgpu::TextureView {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if ((x / 64 + y / 64 + phase) % 2) == 0 {
                230
            } else {
                25
            };
            data[i] = c;
            data[i + 1] = if phase == 0 { c } else { 60 };
            data[i + 2] = 255 - c;
            data[i + 3] = 255;
        }
    }
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("frametest-tex"),
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
        &data,
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
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

impl App {
    fn init_gpu(&mut self, window: Arc<Window>) {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("request adapter");
        println!("adapter: {:?}", adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("frametest-device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        }))
        .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .expect("surface format");
        let present_mode = if caps.present_modes.contains(&self.present) {
            self.present
        } else {
            println!(
                "requested present mode {:?} unsupported (supported: {:?}); using AutoVsync",
                self.present, caps.present_modes
            );
            wgpu::PresentMode::AutoVsync
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps
                .alpha_modes
                .iter()
                .copied()
                .find(|m| *m == wgpu::CompositeAlphaMode::Opaque)
                .or_else(|| caps.alpha_modes.first().copied())
                .expect("alpha mode"),
            view_formats: vec![],
            desired_maximum_frame_latency: self.latency,
        };
        surface.configure(&device, &config);
        println!(
            "surface: {}x{} format={:?} present={:?} latency={}",
            config.width, config.height, config.format, config.present_mode, self.latency
        );

        let tex_a = checker_texture(&device, &queue, config.width, config.height, 0);
        let tex_b = checker_texture(&device, &queue, config.width, config.height, 1);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frametest-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frametest-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frametest-bind"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&tex_a),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&tex_b),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("frametest-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(SHADER)),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("frametest-pipeline-layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("frametest-pipeline"),
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
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });

        self.gpu = Some(Gpu {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind,
            params_buf,
        });
    }

    fn draw(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else { return };
        let frame = match gpu.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(err) => {
                println!("get_current_texture failed: {err:?}; reconfiguring");
                gpu.surface.configure(&gpu.device, &gpu.config);
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let t = self.started.elapsed().as_secs_f32();
        let params: [f32; 4] = [t, f32::from_bits(self.mode), 0.0, 0.0];
        gpu.queue
            .write_buffer(&gpu.params_buf, 0, bytemuck::bytes_of(&params));
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frametest"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frametest-pass"),
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
            rpass.set_bind_group(0, &gpu.bind, &[]);
            rpass.draw(0..3, 0..1);
        }
        gpu.queue.submit(Some(encoder.finish()));
        frame.present();

        let now = Instant::now();
        let dt_ms = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        if self.frames > 0 {
            self.worst_ms = self.worst_ms.max(dt_ms);
            self.best_ms = self.best_ms.min(dt_ms);
        }
        self.frames += 1;
        let span = now.duration_since(self.window_start).as_secs_f32();
        if span >= 1.0 && self.frames > 1 {
            let intervals = (self.frames - 1) as f32;
            println!(
                "fps {:6.1}  avg {:6.2}ms  best {:6.2}ms  worst {:6.2}ms  ({} frames)",
                intervals / span,
                1000.0 * span / intervals,
                self.best_ms,
                self.worst_ms,
                self.frames
            );
            self.window_start = now;
            self.frames = 0;
            self.worst_ms = 0.0;
            self.best_ms = f32::INFINITY;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("frametest")
            .with_fullscreen(Some(Fullscreen::Borderless(None)));
        #[allow(unused_mut)]
        let mut attrs = attrs;
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = attrs.with_name("photoframe", "photoframe");
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        self.init_gpu(window.clone());
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    println!("resized to {}x{}", gpu.config.width, gpu.config.height);
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw();
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = match args.get(1).map(String::as_str) {
        Some("solid") => 0u32,
        Some("tex") => 1,
        None | Some("fade") => 2,
        Some(other) => {
            eprintln!("unknown mode {other:?}; use solid|tex|fade");
            std::process::exit(2);
        }
    };
    let present = match args.get(2).map(String::as_str) {
        None | Some("fifo") => wgpu::PresentMode::Fifo,
        Some("mailbox") => wgpu::PresentMode::Mailbox,
        Some("immediate") => wgpu::PresentMode::Immediate,
        Some(other) => {
            eprintln!("unknown present mode {other:?}; use fifo|mailbox|immediate");
            std::process::exit(2);
        }
    };
    let latency: u32 = args
        .get(3)
        .map(|s| s.parse().expect("latency must be an integer"))
        .unwrap_or(2);
    println!("frametest: mode={mode} present={present:?} latency={latency} (Ctrl-C to quit)");

    let event_loop = EventLoop::new().expect("event loop");
    let mut app = App {
        mode,
        present,
        latency,
        window: None,
        gpu: None,
        started: Instant::now(),
        window_start: Instant::now(),
        frames: 0,
        worst_ms: 0.0,
        best_ms: f32::INFINITY,
        last_frame: Instant::now(),
    };
    event_loop.run_app(&mut app).expect("run app");
}
