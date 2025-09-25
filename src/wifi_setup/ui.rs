use super::{SetupScreenInfo, WifiSetupStatus};
use anyhow::{Context, Result};
use qrcode::QrCode;
use std::sync::{mpsc::Receiver, Arc};
use tracing::{info, warn};
use wgpu::util::DeviceExt;
use wgpu_glyph::{ab_glyph::FontArc, GlyphBrush, GlyphBrushBuilder, Section, Text};
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Fullscreen, Window, WindowAttributes};

pub enum UiControl {
    Exit,
}

pub fn run(
    info: SetupScreenInfo,
    status_rx: Receiver<WifiSetupStatus>,
    ctrl_rx: Receiver<UiControl>,
) -> Result<()> {
    let event_loop = EventLoop::new().context("failed to create event loop")?;
    let mut app = WifiSetupApp::new(info, status_rx, ctrl_rx);
    event_loop
        .run_app(&mut app)
        .context("wifi setup UI loop failed")
}

struct WifiSetupApp {
    info: SetupScreenInfo,
    status_rx: Receiver<WifiSetupStatus>,
    ctrl_rx: Receiver<UiControl>,
    state: Option<UiState>,
    latest_status: WifiSetupStatus,
}

impl WifiSetupApp {
    fn new(
        info: SetupScreenInfo,
        status_rx: Receiver<WifiSetupStatus>,
        ctrl_rx: Receiver<UiControl>,
    ) -> Self {
        Self {
            info,
            status_rx,
            ctrl_rx,
            state: None,
            latest_status: WifiSetupStatus::StartingHotspot,
        }
    }
}

impl winit::application::ApplicationHandler for WifiSetupApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match UiState::new(event_loop, self.info.clone()) {
            Ok(mut state) => {
                info!("wifi setup UI ready");
                state.update_status(self.latest_status.clone());
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(err) => {
                warn!("failed to initialize wifi setup UI: {err:?}");
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(ctrl) = self.ctrl_rx.try_recv() {
            if matches!(ctrl, UiControl::Exit) {
                event_loop.exit();
                return;
            }
        }
        let mut updated = false;
        while let Ok(status) = self.status_rx.try_recv() {
            self.latest_status = status.clone();
            if let Some(state) = self.state.as_mut() {
                state.update_status(status);
                updated = true;
            }
        }
        if let Some(state) = self.state.as_ref() {
            if updated {
                state.window.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(err) = state.render() {
                    match err {
                        RenderError::Surface(wgpu::SurfaceError::Lost)
                        | RenderError::Surface(wgpu::SurfaceError::Outdated) => {
                            state.resize(state.window.inner_size());
                        }
                        RenderError::Surface(wgpu::SurfaceError::OutOfMemory) => {
                            warn!("surface out of memory; exiting setup UI");
                            event_loop.exit();
                        }
                        RenderError::Surface(other) => {
                            warn!("surface error: {other:?}");
                        }
                        RenderError::Glyph(gerr) => {
                            warn!("glyph error during render: {gerr:?}");
                        }
                    }
                }
            }
            WindowEvent::Resized(size) => {
                state.resize(size);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged {
                mut inner_size_writer,
                ..
            } => {
                let size = state.window.inner_size();
                let _ = inner_size_writer.request_inner_size(size);
                state.resize(size);
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}

struct UiState {
    window: Arc<Window>,
    renderer: Renderer,
    info: SetupScreenInfo,
    status: WifiSetupStatus,
    lines: Vec<DisplayLine>,
}

impl UiState {
    fn new(event_loop: &ActiveEventLoop, info: SetupScreenInfo) -> Result<Self> {
        let attrs = WindowAttributes::default()
            .with_title("Frame Wi-Fi Setup")
            .with_fullscreen(Some(Fullscreen::Borderless(None)));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .context("failed to create wifi setup window")?,
        );
        let renderer = Renderer::new(window.clone(), &info)?;
        let mut state = Self {
            window,
            renderer,
            info,
            status: WifiSetupStatus::StartingHotspot,
            lines: Vec::new(),
        };
        state.update_status(WifiSetupStatus::StartingHotspot);
        Ok(state)
    }

    fn update_status(&mut self, status: WifiSetupStatus) {
        self.status = status.clone();
        self.lines = compose_lines(&self.info, &self.status);
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.renderer.resize(size);
    }

    fn render(&mut self) -> Result<(), RenderError> {
        self.renderer.render(&self.lines)
    }
}

#[derive(Clone)]
struct DisplayLine {
    text: String,
    scale: f32,
    color: [f32; 4],
}

impl DisplayLine {
    fn new<T: Into<String>>(text: T, scale: f32, color: [f32; 4]) -> Self {
        Self {
            text: text.into(),
            scale,
            color,
        }
    }
}

fn compose_lines(info: &SetupScreenInfo, status: &WifiSetupStatus) -> Vec<DisplayLine> {
    let mut lines = Vec::new();
    lines.push(DisplayLine::new(
        "Frame Wi-Fi Setup",
        44.0,
        [0.05, 0.05, 0.05, 1.0],
    ));
    lines.push(DisplayLine::new("", 18.0, [0.05, 0.05, 0.05, 1.0]));
    lines.push(DisplayLine::new(
        format!("Hotspot: {}", info.hotspot_ssid),
        28.0,
        [0.1, 0.1, 0.1, 1.0],
    ));
    lines.push(DisplayLine::new(
        format!("Password: {}", info.hotspot_password),
        28.0,
        [0.1, 0.1, 0.1, 1.0],
    ));
    lines.push(DisplayLine::new("", 18.0, [0.05, 0.05, 0.05, 1.0]));

    if info.access_urls.is_empty() {
        lines.push(DisplayLine::new(
            "Open http://192.168.4.1:8080",
            26.0,
            [0.12, 0.12, 0.12, 1.0],
        ));
    } else {
        lines.push(DisplayLine::new(
            "Visit one of these URLs:",
            26.0,
            [0.12, 0.12, 0.12, 1.0],
        ));
        for url in &info.access_urls {
            lines.push(DisplayLine::new(url.clone(), 26.0, [0.1, 0.1, 0.1, 1.0]));
        }
    }
    lines.push(DisplayLine::new("", 18.0, [0.05, 0.05, 0.05, 1.0]));

    let (status_text, color) = match status {
        WifiSetupStatus::StartingHotspot => {
            ("Preparing hotspot...".to_string(), [0.1, 0.3, 0.6, 1.0])
        }
        WifiSetupStatus::WaitingForCredentials => {
            ("Ready for Wi-Fi details.".to_string(), [0.1, 0.3, 0.6, 1.0])
        }
        WifiSetupStatus::ApplyingCredentials { ssid } => {
            (format!("Connecting to '{ssid}'..."), [0.1, 0.35, 0.7, 1.0])
        }
        WifiSetupStatus::ConnectionFailed { ssid, message } => {
            let msg = if ssid.is_empty() {
                format!("Setup error: {message}")
            } else {
                format!("Failed to connect to '{ssid}': {message}")
            };
            (msg, [0.75, 0.2, 0.2, 1.0])
        }
        WifiSetupStatus::Connected { ssid } => (
            format!("Connected to '{ssid}'. Restarting..."),
            [0.15, 0.55, 0.25, 1.0],
        ),
    };
    lines.push(DisplayLine::new(status_text, 30.0, color));
    lines
}

struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    glyph_brush: GlyphBrush<()>,
    staging_belt: wgpu::util::StagingBelt,
    qr_mesh: QrMesh,
    vertex_buffer: Option<wgpu::Buffer>,
    vertex_count: u32,
}

impl Renderer {
    fn new(window: Arc<Window>, info: &SetupScreenInfo) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .context("failed to create surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("failed to request adapter")?;
        let limits = wgpu::Limits::downlevel_defaults();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wifi-setup-device"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        }))
        .context("failed to request device")?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(capabilities.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wifi-setup-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("qr_shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wifi-setup-pipeline-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wifi-setup-qr-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
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

        let font =
            FontArc::try_from_slice(include_bytes!("../../assets/fonts/Inconsolata-Regular.ttf"))
                .context("failed to load UI font")?;
        let glyph_brush = GlyphBrushBuilder::using_font(font).build(&device, config.format);
        let qr_url = info
            .access_urls
            .first()
            .cloned()
            .unwrap_or_else(|| "http://192.168.4.1:8080".to_string());
        let qr_mesh = QrMesh::new(&qr_url)?;

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            glyph_brush,
            staging_belt: wgpu::util::StagingBelt::new(1024),
            qr_mesh,
            vertex_buffer: None,
            vertex_count: 0,
        };
        renderer.rebuild_vertices(size);
        Ok(renderer)
    }

    fn rebuild_vertices(&mut self, size: PhysicalSize<u32>) {
        let vertices = self.qr_mesh.build_vertices(size.width, size.height);
        if vertices.is_empty() {
            self.vertex_buffer = None;
            self.vertex_count = 0;
            return;
        }
        self.vertex_count = vertices.len() as u32;
        self.vertex_buffer = Some(self.device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("wifi-setup-qr-verts"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            },
        ));
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.rebuild_vertices(size);
    }

    fn render(&mut self, lines: &[DisplayLine]) -> Result<(), RenderError> {
        let frame = self
            .surface
            .get_current_texture()
            .map_err(RenderError::Surface)?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wifi-setup-encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wifi-setup-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.98,
                            g: 0.98,
                            b: 0.98,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(buffer) = &self.vertex_buffer {
                rpass.set_pipeline(&self.pipeline);
                rpass.set_vertex_buffer(0, buffer.slice(..));
                rpass.draw(0..self.vertex_count, 0..1);
            }
        }
        self.queue_text(lines);
        self.glyph_brush
            .draw_queued(
                &self.device,
                &mut self.staging_belt,
                &mut encoder,
                &view,
                self.config.width,
                self.config.height,
            )
            .map_err(RenderError::Glyph)?;
        self.staging_belt.finish();
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.staging_belt.recall();
        Ok(())
    }

    fn queue_text(&mut self, lines: &[DisplayLine]) {
        let text_x =
            (self.config.width as f32 * 0.55).clamp(40.0, self.config.width as f32 - 200.0);
        let mut cursor_y =
            (self.config.height as f32 * 0.18).clamp(40.0, self.config.height as f32 - 200.0);
        for line in lines {
            self.glyph_brush.queue(Section {
                screen_position: (text_x, cursor_y),
                bounds: (self.config.width as f32 * 0.4, self.config.height as f32),
                text: vec![Text::new(&line.text)
                    .with_scale(line.scale)
                    .with_color(line.color)],
                ..Section::default()
            });
            cursor_y += line.scale + 10.0;
        }
    }
}

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 3],
}

const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x3];

impl Vertex {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &VERTEX_ATTRIBUTES,
        }
    }
}

struct QrMesh {
    width: usize,
    modules: Vec<bool>,
}

impl QrMesh {
    fn new(url: &str) -> Result<Self> {
        let code = QrCode::new(url.as_bytes()).context("failed to generate QR code")?;
        let width = code.width();
        let modules = code
            .to_colors()
            .into_iter()
            .map(|color| matches!(color, qrcode::types::Color::Dark))
            .collect::<Vec<_>>();
        Ok(Self { width, modules })
    }

    fn build_vertices(&self, width: u32, height: u32) -> Vec<Vertex> {
        if self.modules.is_empty() || self.width == 0 {
            return Vec::new();
        }
        let width_f = width.max(1) as f32;
        let height_f = height.max(1) as f32;
        let max_qr_width = (width_f * 0.45).max(120.0);
        let max_qr_height = (height_f * 0.8).max(120.0);
        let module_size = (max_qr_width.min(max_qr_height) / self.width as f32).max(2.0);
        let total_size = module_size * self.width as f32;
        let mut start_x = (width_f * 0.1).max(20.0);
        if start_x + total_size > width_f * 0.5 {
            start_x = (width_f * 0.5 - total_size).max(20.0);
        }
        let mut start_y = ((height_f - total_size) / 2.0).max(20.0);
        if start_y + total_size > height_f - 20.0 {
            start_y = (height_f - total_size - 20.0).max(20.0);
        }
        let mut vertices = Vec::with_capacity(self.modules.len() * 6);
        for y in 0..self.width {
            for x in 0..self.width {
                if !self.modules[y * self.width + x] {
                    continue;
                }
                let px = start_x + x as f32 * module_size;
                let py = start_y + y as f32 * module_size;
                let x1 = px + module_size;
                let y1 = py + module_size;
                let tl = to_ndc(px, py, width_f, height_f);
                let bl = to_ndc(px, y1, width_f, height_f);
                let tr = to_ndc(x1, py, width_f, height_f);
                let br = to_ndc(x1, y1, width_f, height_f);
                let color = [0.05, 0.05, 0.05];
                vertices.push(Vertex {
                    position: tl,
                    color,
                });
                vertices.push(Vertex {
                    position: bl,
                    color,
                });
                vertices.push(Vertex {
                    position: br,
                    color,
                });
                vertices.push(Vertex {
                    position: tl,
                    color,
                });
                vertices.push(Vertex {
                    position: br,
                    color,
                });
                vertices.push(Vertex {
                    position: tr,
                    color,
                });
            }
        }
        vertices
    }
}

fn to_ndc(x: f32, y: f32, width: f32, height: f32) -> [f32; 2] {
    [(x / width) * 2.0 - 1.0, 1.0 - (y / height) * 2.0]
}

enum RenderError {
    Surface(wgpu::SurfaceError),
    Glyph(String),
}
