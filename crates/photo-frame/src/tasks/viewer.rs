mod scenes;
mod state;

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use scenes::{
    RenderCtx, RenderResult, Scene, SceneContext, ScenePresentEvent, asleep::AsleepScene,
    awake::AwakeScene, greeting::GreetingScene,
};
use state::{ViewerSM, ViewerState, ViewerStateChange};
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
    Tick(Instant),
    Command(ViewerCommand),
    Photo(PhotoLoaded),
}

type PhotoReceiver = mpsc::Receiver<PhotoLoaded>;
type DisplayedSender = mpsc::Sender<Displayed>;
type CommandReceiver = mpsc::Receiver<ViewerCommand>;

struct Scenes {
    greeting: GreetingScene,
    awake: AwakeScene,
    asleep: AsleepScene,
}

struct GpuCtx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scenes: Scenes,
}

struct ViewerApp {
    cfg: Configuration,
    cancel: CancellationToken,
    window: Option<Arc<Window>>,
    gpu: Option<GpuCtx>,
    state: ViewerState,
    sm: ViewerSM,
    pending_redraw: bool,
    pending_photos: VecDeque<PhotoLoaded>,
    to_manager_displayed: DisplayedSender,
}

impl ViewerApp {
    fn new(
        cfg: Configuration,
        cancel: CancellationToken,
        to_manager_displayed: DisplayedSender,
    ) -> Self {
        let now = Instant::now();
        let sm = ViewerSM::new(cfg.greeting_screen.effective_duration(), now);
        info!(
            greeting_duration_ms = cfg.greeting_screen.effective_duration().as_millis(),
            "viewer_app_created"
        );
        let state = sm.current();
        Self {
            cfg,
            cancel,
            window: None,
            gpu: None,
            state,
            sm,
            pending_redraw: false,
            pending_photos: VecDeque::new(),
            to_manager_displayed,
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

        let scenes = Scenes {
            greeting: GreetingScene::new(&device, &queue, format, &self.cfg.greeting_screen),
            awake: AwakeScene::new(),
            asleep: AsleepScene::new(&device, &queue, format, &self.cfg.sleep_screen),
        };

        self.gpu = Some(GpuCtx {
            surface,
            device,
            queue,
            config,
            scenes,
        });
        self.pending_redraw = true;

        self.enter_current_state();
        Ok(())
    }

    fn enter_current_state(&mut self) {
        let (gpu, window) = match (self.gpu.as_mut(), self.window.as_ref()) {
            (Some(gpu), Some(window)) => (gpu, window.as_ref()),
            _ => return,
        };
        match self.state {
            ViewerState::Greeting => {
                let ctx = SceneContext {
                    device: &gpu.device,
                    queue: &gpu.queue,
                    surface_config: &gpu.config,
                    window,
                };
                info!("viewer_enter_greeting");
                gpu.scenes.greeting.on_enter(&ctx);
            }
            ViewerState::Awake => {
                let ctx = SceneContext {
                    device: &gpu.device,
                    queue: &gpu.queue,
                    surface_config: &gpu.config,
                    window,
                };
                info!("viewer_enter_awake");
                gpu.scenes.awake.on_enter(&ctx);
                self.advance_photo_queue();
            }
            ViewerState::Asleep => {
                let ctx = SceneContext {
                    device: &gpu.device,
                    queue: &gpu.queue,
                    surface_config: &gpu.config,
                    window,
                };
                info!("viewer_enter_asleep");
                gpu.scenes.asleep.on_enter(&ctx);
            }
        }
    }

    fn handle_resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        let (gpu, window) = match (self.gpu.as_mut(), self.window.as_ref()) {
            (Some(gpu), Some(window)) => (gpu, window.as_ref()),
            _ => return,
        };

        gpu.config.width = new_size.width.max(1);
        gpu.config.height = new_size.height.max(1);
        gpu.surface.configure(&gpu.device, &gpu.config);
        debug!(
            width = gpu.config.width,
            height = gpu.config.height,
            "viewer_surface_configured"
        );
        let scale_factor = window.scale_factor();
        {
            let ctx = SceneContext {
                device: &gpu.device,
                queue: &gpu.queue,
                surface_config: &gpu.config,
                window,
            };
            gpu.scenes
                .greeting
                .handle_resize(&ctx, new_size, scale_factor);
        }
        {
            let ctx = SceneContext {
                device: &gpu.device,
                queue: &gpu.queue,
                surface_config: &gpu.config,
                window,
            };
            gpu.scenes.awake.handle_resize(&ctx, new_size, scale_factor);
        }
        {
            let ctx = SceneContext {
                device: &gpu.device,
                queue: &gpu.queue,
                surface_config: &gpu.config,
                window,
            };
            gpu.scenes
                .asleep
                .handle_resize(&ctx, new_size, scale_factor);
        }

        self.request_redraw();
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let should_draw = std::mem::take(&mut self.pending_redraw);
        if !should_draw {
            debug!("viewer_draw_skipped");
            return;
        }

        let (pending_event, needs_redraw) = {
            let (gpu, window) = match (self.gpu.as_mut(), self.window.as_ref()) {
                (Some(gpu), Some(window)) => (gpu, window.as_ref()),
                _ => return,
            };

            let frame = match gpu.surface.get_current_texture() {
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
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewer-encoder"),
                });

            let render_result = match self.state {
                ViewerState::Greeting => {
                    let mut render_ctx = RenderCtx {
                        scene: SceneContext {
                            device: &gpu.device,
                            queue: &gpu.queue,
                            surface_config: &gpu.config,
                            window,
                        },
                        encoder: &mut encoder,
                        target_view: &view,
                    };
                    gpu.scenes.greeting.render(&mut render_ctx)
                }
                ViewerState::Awake => {
                    let mut render_ctx = RenderCtx {
                        scene: SceneContext {
                            device: &gpu.device,
                            queue: &gpu.queue,
                            surface_config: &gpu.config,
                            window,
                        },
                        encoder: &mut encoder,
                        target_view: &view,
                    };
                    gpu.scenes.awake.render(&mut render_ctx)
                }
                ViewerState::Asleep => {
                    let mut render_ctx = RenderCtx {
                        scene: SceneContext {
                            device: &gpu.device,
                            queue: &gpu.queue,
                            surface_config: &gpu.config,
                            window,
                        },
                        encoder: &mut encoder,
                        target_view: &view,
                    };
                    gpu.scenes.asleep.render(&mut render_ctx)
                }
            };

            let needs_redraw = matches!(render_result, RenderResult::NeedsRedraw);

            gpu.queue.submit(std::iter::once(encoder.finish()));
            frame.present();

            let scene_ctx = SceneContext {
                device: &gpu.device,
                queue: &gpu.queue,
                surface_config: &gpu.config,
                window,
            };
            let event = match self.state {
                ViewerState::Greeting => gpu.scenes.greeting.after_present(&scene_ctx),
                ViewerState::Awake => gpu.scenes.awake.after_present(&scene_ctx),
                ViewerState::Asleep => gpu.scenes.asleep.after_present(&scene_ctx),
            };

            (event, needs_redraw)
        };

        debug!(state = ?self.state, needs_redraw, has_event = pending_event.is_some(), "viewer_draw_complete");

        if needs_redraw {
            self.request_redraw();
        }
        if let Some(event) = pending_event {
            self.handle_scene_event(event);
        }
    }

    fn handle_scene_event(&mut self, event: ScenePresentEvent) {
        match event {
            ScenePresentEvent::PhotoDisplayed(path) => {
                debug!(path = %path.display(), "viewer_photo_displayed_event");
                self.notify_displayed(path)
            }
        }
    }

    fn notify_displayed(&mut self, path: std::path::PathBuf) {
        if let Err(err) = self.to_manager_displayed.try_send(Displayed(path.clone())) {
            warn!(error = %err, photo = %path.display(), "failed to forward displayed notification");
        }
    }

    fn request_redraw(&mut self) {
        self.pending_redraw = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn apply_state_change(&mut self, change: ViewerStateChange) {
        let previous = self.state;
        self.state = change.to;
        info!(from = ?change.from, to = ?change.to, "viewer state transition");

        if let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) {
            let ctx = SceneContext {
                device: &gpu.device,
                queue: &gpu.queue,
                surface_config: &gpu.config,
                window: window.as_ref(),
            };
            match previous {
                ViewerState::Greeting => gpu.scenes.greeting.on_exit(&ctx),
                ViewerState::Awake => gpu.scenes.awake.on_exit(&ctx),
                ViewerState::Asleep => gpu.scenes.asleep.on_exit(&ctx),
            }
            match self.state {
                ViewerState::Greeting => gpu.scenes.greeting.on_enter(&ctx),
                ViewerState::Awake => {
                    gpu.scenes.awake.on_enter(&ctx);
                    self.advance_photo_queue();
                }
                ViewerState::Asleep => gpu.scenes.asleep.on_enter(&ctx),
            }
        }

        self.request_redraw();
    }

    fn advance_photo_queue(&mut self) {
        if !matches!(self.state, ViewerState::Awake) {
            return;
        }
        if let Some(photo) = self.pending_photos.pop_front() {
            debug!(path = %photo.prepared.path.display(), "advance_photo_queue_displaying");
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.scenes.awake.queue_photo(photo);
            }
            self.request_redraw();
        } else {
            debug!("advance_photo_queue_empty");
        }
    }

    fn on_photo_loaded(&mut self, photo: PhotoLoaded, now: Instant) {
        debug!(path = %photo.prepared.path.display(), priority = photo.priority, "viewer_photo_loaded");
        self.pending_photos.push_back(photo);
        if let Some(change) = self.sm.on_photo_ready(now) {
            self.apply_state_change(change);
        } else if matches!(self.state, ViewerState::Awake) {
            self.advance_photo_queue();
        }
    }

    fn on_command(&mut self, command: ViewerCommand, now: Instant) {
        debug!(?command, "viewer_command_received");
        if let Some(change) = self.sm.on_command(&command, now) {
            self.apply_state_change(change);
        }
    }

    fn on_tick(&mut self, now: Instant) {
        debug!(state = ?self.state, "viewer_tick");
        if let Some(change) = self.sm.on_tick(now) {
            self.apply_state_change(change);
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

        if self.gpu.is_none() {
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
            ViewerEvent::Tick(now) => self.on_tick(now),
            ViewerEvent::Command(cmd) => self.on_command(cmd, Instant::now()),
            ViewerEvent::Photo(photo) => self.on_photo_loaded(photo, Instant::now()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_windowed(
    mut from_loader: PhotoReceiver,
    to_manager_displayed: DisplayedSender,
    cancel: CancellationToken,
    cfg: Configuration,
    mut control: CommandReceiver,
) -> Result<()> {
    let event_loop = EventLoop::<ViewerEvent>::with_user_event()
        .build()
        .context("failed to build viewer event loop")?;
    let proxy = event_loop.create_proxy();
    info!("viewer_event_loop_started");

    let cancel_task = {
        let cancel = cancel.clone();
        let proxy = proxy.clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            let _ = proxy.send_event(ViewerEvent::Cancelled);
        })
    };

    let loader_task = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            while let Some(photo) = from_loader.recv().await {
                if proxy.send_event(ViewerEvent::Photo(photo)).is_err() {
                    break;
                }
            }
        })
    };

    let command_task = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            while let Some(cmd) = control.recv().await {
                if proxy.send_event(ViewerEvent::Command(cmd)).is_err() {
                    break;
                }
            }
        })
    };

    let tick_task = {
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(33));
            loop {
                ticker.tick().await;
                if proxy.send_event(ViewerEvent::Tick(Instant::now())).is_err() {
                    break;
                }
            }
        })
    };

    let mut app = ViewerApp::new(cfg, cancel, to_manager_displayed);
    let run_result = event_loop.run_app(&mut app);

    cancel_task.abort();
    loader_task.abort();
    command_task.abort();
    tick_task.abort();

    run_result.context("viewer event loop failed")
}
