mod scenes;
mod state;

use std::sync::Arc;

use anyhow::{Context, Result};
use scenes::{RenderCtx, RenderResult, Scene, SceneContext, greeting::GreetingScene};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use wgpu::{self, SurfaceError};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowAttributes},
};

use crate::{
    config::Configuration,
    events::{Displayed, PhotoLoaded, ViewerCommand},
};

#[derive(Debug)]
enum ViewerEvent {
    Cancelled,
}

type PhotoReceiver = mpsc::Receiver<PhotoLoaded>;
type DisplayedSender = mpsc::Sender<Displayed>;
type CommandReceiver = mpsc::Receiver<ViewerCommand>;

struct ViewerApp {
    cfg: Configuration,
    cancel: CancellationToken,
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    surface_config: Option<wgpu::SurfaceConfiguration>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    greeting_scene: Option<GreetingScene>,
    pending_redraw: bool,
    _from_loader: PhotoReceiver,
    _to_manager_displayed: DisplayedSender,
    _control: CommandReceiver,
}

impl ViewerApp {
    fn new(
        cfg: Configuration,
        cancel: CancellationToken,
        from_loader: PhotoReceiver,
        to_manager_displayed: DisplayedSender,
        control: CommandReceiver,
    ) -> Self {
        Self {
            cfg,
            cancel,
            window: None,
            surface: None,
            surface_config: None,
            device: None,
            queue: None,
            greeting_scene: None,
            pending_redraw: false,
            _from_loader: from_loader,
            _to_manager_displayed: to_manager_displayed,
            _control: control,
        }
    }

    fn ensure_window(&mut self, event_loop: &ActiveEventLoop) -> Option<Arc<Window>> {
        if let Some(window) = self.window.as_ref() {
            return Some(window.clone());
        }

        let attrs = WindowAttributes::default().with_title("Rust Photo Frame");
        match event_loop.create_window(attrs) {
            Ok(window) => {
                let window = Arc::new(window);
                self.window = Some(window.clone());
                Some(window)
            }
            Err(err) => {
                error!(error = %err, "failed to create viewer window");
                None
            }
        }
    }

    fn init_gpu(&mut self, window: Arc<Window>) -> Result<()> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .context("failed to create surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("failed to acquire GPU adapter")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|fmt| fmt.is_srgb())
            .unwrap_or(caps.formats[0]);

        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("viewer-device"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        }))
        .context("failed to acquire GPU device")?;

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
        info!(
            width = config.width,
            height = config.height,
            format = ?config.format,
            "viewer surface configured",
        );

        let greeting = GreetingScene::new(&device, &queue, format, &self.cfg.greeting_screen);

        self.surface = Some(surface);
        self.surface_config = Some(config);
        self.device = Some(device);
        self.queue = Some(queue);
        self.greeting_scene = Some(greeting);
        self.pending_redraw = true;

        if let (Some(window), Some(scene)) = (self.window.as_ref(), self.greeting_scene.as_mut()) {
            if let (Some(device), Some(queue), Some(config)) = (
                self.device.as_ref(),
                self.queue.as_ref(),
                self.surface_config.as_ref(),
            ) {
                let ctx = SceneContext {
                    device,
                    queue,
                    surface_config: config,
                    window: window.as_ref(),
                };
                scene.on_enter(&ctx);
            }
        }

        Ok(())
    }

    fn handle_resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        let surface = match self.surface.as_ref() {
            Some(surface) => surface,
            None => return,
        };
        let device = match self.device.as_ref() {
            Some(device) => device,
            None => return,
        };
        let queue = match self.queue.as_ref() {
            Some(queue) => queue,
            None => return,
        };
        let window = match self.window.as_ref() {
            Some(window) => window.as_ref(),
            None => return,
        };
        let config = match self.surface_config.as_mut() {
            Some(config) => config,
            None => return,
        };

        config.width = new_size.width.max(1);
        config.height = new_size.height.max(1);
        surface.configure(device, config);
        debug!(
            width = config.width,
            height = config.height,
            "viewer surface resized",
        );

        if let Some(scene) = self.greeting_scene.as_mut() {
            let ctx = SceneContext {
                device,
                queue,
                surface_config: config,
                window,
            };
            scene.handle_resize(&ctx, new_size, window.scale_factor());
        }

        self.request_redraw();
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let surface = match self.surface.as_ref() {
            Some(surface) => surface,
            None => return,
        };
        let device = match self.device.as_ref() {
            Some(device) => device,
            None => return,
        };
        let queue = match self.queue.as_ref() {
            Some(queue) => queue,
            None => return,
        };
        let config = match self.surface_config.as_ref() {
            Some(config) => config,
            None => return,
        };
        let window = match self.window.as_ref() {
            Some(window) => window.as_ref(),
            None => return,
        };

        let frame = match surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Outdated) | Err(SurfaceError::Lost) => {
                info!("viewer surface lost; reconfiguring");
                self.handle_resize(window.inner_size());
                return;
            }
            Err(SurfaceError::OutOfMemory) => {
                error!("viewer surface out of memory; exiting event loop");
                event_loop.exit();
                return;
            }
            Err(SurfaceError::Timeout) => {
                warn!("viewer surface acquisition timed out");
                return;
            }
            Err(SurfaceError::Other) => {
                warn!("viewer surface reported an unknown error; retrying");
                self.handle_resize(window.inner_size());
                return;
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-encoder"),
        });

        self.pending_redraw = false;

        if let Some(scene) = self.greeting_scene.as_mut() {
            let ctx = SceneContext {
                device,
                queue,
                surface_config: config,
                window,
            };
            let mut render_ctx = RenderCtx {
                scene: ctx,
                encoder: &mut encoder,
                target_view: &view,
            };
            if matches!(scene.render(&mut render_ctx), RenderResult::NeedsRedraw) {
                self.pending_redraw = true;
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }

    fn request_redraw(&mut self) {
        self.pending_redraw = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler<ViewerEvent> for ViewerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.cancel.is_cancelled() {
            event_loop.exit();
            return;
        }

        let Some(window) = self.ensure_window(event_loop) else {
            event_loop.exit();
            return;
        };

        if self.device.is_none() {
            if let Err(err) = self.init_gpu(window) {
                error!(error = ?err, "failed to initialize GPU state");
                event_loop.exit();
                return;
            }
        }

        self.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                info!("viewer window close requested");
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resize(new_size);
            }
            WindowEvent::ScaleFactorChanged {
                mut inner_size_writer,
                ..
            } => {
                let size = window.inner_size();
                let _ = inner_size_writer.request_inner_size(size);
                self.handle_resize(size);
            }
            WindowEvent::RedrawRequested => {
                self.draw(event_loop);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.pending_redraw {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: ViewerEvent) {
        match event {
            ViewerEvent::Cancelled => {
                info!("viewer received cancellation event");
                event_loop.exit();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_windowed(
    from_loader: PhotoReceiver,
    to_manager_displayed: DisplayedSender,
    cancel: CancellationToken,
    cfg: Configuration,
    control: CommandReceiver,
) -> Result<()> {
    let event_loop = EventLoop::<ViewerEvent>::with_user_event()
        .build()
        .context("failed to build viewer event loop")?;
    let proxy = event_loop.create_proxy();

    let cancel_task = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            let _ = proxy.send_event(ViewerEvent::Cancelled);
        })
    };

    let mut app = ViewerApp::new(cfg, cancel, from_loader, to_manager_displayed, control);
    let run_result = event_loop.run_app(&mut app);
    cancel_task.abort();

    run_result.context("viewer event loop failed")
}
