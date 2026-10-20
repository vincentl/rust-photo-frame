//! The viewer on DRM/KMS, with no compositor (`PHOTOFRAME_SURFACE=drm`).
//!
//! Launched by `photoframe-kms.service` instead of the greetd → sway kiosk
//! session. It runs the same `App` as the Wayland path (scenes, matting,
//! uploads, `redraw()`); only the output and the loop differ:
//!
//! * **Output.** The card is opened and DRM master asserted *before* the
//!   wgpu `Instance` exists, then the surface is created from
//!   `SurfaceTargetUnsafe::Drm` (`crate::drm_display`, which documents why
//!   that order matters). The mode is fixed for the process lifetime, so
//!   the surface is configured once and marked ready immediately; the
//!   `SurfaceReadyGate` exists for compositor configure churn that KMS does
//!   not have.
//! * **Loop.** No winit and no 4 ms ticker. Each iteration drains control
//!   commands, runs `process_tick()`, and renders if a scene asked for a
//!   frame. With FIFO (the KMS default) `get_current_texture` blocks until a
//!   swapchain image is free, i.e. until vblank, so a transition renders one
//!   frame per refresh with no other clock. When nothing needs drawing the
//!   loop sleeps in `wake::next_wake_event` until a control command, a
//!   matting result, a decoded photo (only while the preload queue is
//!   short), or the dwell deadline. See `wake.rs`.
//! * **Runtime display changes (v1.2: not handled).** A lost or outdated
//!   swapchain (connector unplugged or re-plugged with another mode), any
//!   other surface error, or a streak of acquire timeouts ends the process
//!   with an error; `Restart=always` brings it back and the restart
//!   re-enumerates the card, connector and preferred mode. Losing DRM master
//!   at runtime would need logind or VT switching, which this stack does not
//!   use; in practice "not master" is a startup failure (greetd still
//!   running), retried by systemd until its StartLimit and then left failed.
//! * **Shutdown.** Every exit (SIGTERM/SIGINT/stdin via the cancel token,
//!   the control or matting channel closing, a fatal surface error) returns
//!   through `run_kms`, which tears down in the only safe order: GPU objects
//!   (swapchain) → `Instance` → card. See `kms_teardown`.

use super::wake::{self, WakeEvent};
use super::*;
use crate::drm_display::{self, KmsRequest, KmsTarget, ModeRequest};

/// `PHOTOFRAME_DRM_CONNECTOR`, e.g. `HDMI-A-1`; default: first connected.
const CONNECTOR_ENV: &str = "PHOTOFRAME_DRM_CONNECTOR";
/// `PHOTOFRAME_DRM_MODE`, `WxH[@Hz]`; default: the connector's preferred mode.
const MODE_ENV: &str = "PHOTOFRAME_DRM_MODE";

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn kms_request_from_env() -> anyhow::Result<KmsRequest> {
    let mode = non_empty_env(MODE_ENV)
        .map(|v| v.parse::<ModeRequest>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("{MODE_ENV}: {e}"))?;
    Ok(KmsRequest {
        card: None,
        connector: non_empty_env(CONNECTOR_ENV),
        mode,
        plane: None,
    })
}

/// Run the viewer on DRM/KMS until cancelled or a fatal display error.
pub async fn run_kms(
    from_loader: Receiver<PhotoLoaded>,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
    cfg: Arc<crate::config::Configuration>,
    mut control: Receiver<ViewerCommand>,
) -> anyhow::Result<()> {
    // Declaration order is the reverse of teardown order: if anything below
    // unwinds, Rust drops `app` (swapchain) before `instance` before
    // `target` (card fd), which is the order kms_teardown enforces on the
    // normal path.
    let request = kms_request_from_env()?;
    let mut trace = |line: &str| info!("kms_probe: {line}");
    // 1. Card first, so this process (not v3dv's own open) holds master.
    let target = drm_display::open_target(&request, &mut trace)?;
    info!(target = %target.describe(), "kms_target");
    target.assert_master()?;
    // 2. Only now the wgpu instance.
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
    // SAFETY: `target` (the card fd inside the surface target) outlives
    // `instance` and everything created from it; see kms_teardown.
    let surface_target = unsafe { drm_display::surface_target(&instance, &target, &mut trace) }?;
    let surface = unsafe { instance.create_surface_unsafe(surface_target) }
        .map_err(|e| anyhow::anyhow!("create DRM surface: {e}"))?;
    let size = PhysicalSize::new(target.width, target.height);
    let parts = build_gpu(&instance, surface, size, PresentDefault::Kms, &cfg, "kms")
        .map_err(|e| anyhow::anyhow!("failed to initialize GPU on KMS: {e}"))?;

    let mut app = App::new(from_loader, to_manager_displayed, cancel, cfg);
    app.enter_greeting();
    app.output = Some(Output::Kms {
        size,
        redraw_requested: Arc::new(AtomicBool::new(false)),
    });
    app.install_gpu(parts, size, "kms");
    app.configure_fixed_surface(size);
    info!(
        width = size.width,
        height = size.height,
        "viewer presenting straight to KMS"
    );

    let result = drive(&mut app, &mut control).await;
    if let Err(err) = &result {
        warn!(error = %err, "kms_viewer_exiting_with_error");
    }
    kms_teardown(app, instance, target);
    result
}

/// The one safe teardown order (crate::drm_display, rule 2): the swapchain
/// and every GPU object go first (dropping `App` drops the surface, the
/// device and the overlay scenes' device handles), then the instance (where
/// Mesa's display WSI joins the vblank wait thread that polls the card fd),
/// and only then the card fd, which also releases DRM master and lets the
/// kernel restore the console. Each step is logged so the journal shows the
/// order on every stop.
fn kms_teardown(app: App, instance: wgpu::Instance, target: KmsTarget) {
    info!("kms_teardown: releasing swapchain and GPU objects");
    drop(app);
    info!("kms_teardown: destroying the wgpu instance");
    drop(instance);
    info!(card = %target.path, "kms_teardown: closing the card (releases DRM master)");
    drop(target);
    info!("kms_teardown: done");
}

impl App {
    /// KMS: apply the surface configuration once and mark it ready. The mode
    /// never changes for this process (a change is a restart, see the
    /// module docs), so there is no configure handshake to wait for.
    fn configure_fixed_surface(&mut self, size: PhysicalSize<u32>) {
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.surface.configure(&gpu.device, &gpu.config);
        }
        let _ = self.surface_gate.set_ready(true);
        self.surface_gate.awaiting = false;
        self.surface_configured = true;
        self.configured_surface_size = Some((size.width, size.height));
    }

    /// Take the KMS redraw request (set by `SceneContext::request_redraw`).
    fn take_kms_redraw_request(&self) -> bool {
        match &self.output {
            Some(Output::Kms {
                redraw_requested, ..
            }) => redraw_requested.swap(false, Ordering::Relaxed),
            _ => false,
        }
    }

    /// One matting result from the awaited channel; same bookkeeping as
    /// `drain_mat_results`.
    fn accept_mat_result(&mut self, result: MatResult) {
        self.mat_inflight = self.mat_inflight.saturating_sub(1);
        if result.priority {
            self.ready_results
                .retain(|existing| existing.path != result.path);
        }
        self.ready_results.push_back(result);
    }
}

async fn drive(app: &mut App, control: &mut Receiver<ViewerCommand>) -> anyhow::Result<()> {
    let mut loader_open = true;
    loop {
        if app.cancel.is_cancelled() {
            info!("kms_viewer_cancelled");
            return Ok(());
        }
        // Commands that arrived while a frame was rendering (at most one
        // refresh of delay).
        loop {
            match control.try_recv() {
                Ok(cmd) => app.handle_control_command(cmd),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    info!("kms_viewer_control_closed");
                    return Ok(());
                }
            }
        }

        app.process_tick();

        // On KMS every requested redraw presents: the surface is always
        // configured and the overlays always have a non-zero size, so a
        // request cannot be left pending without a (vblank-paced) present.
        if app.take_kms_redraw_request() {
            match app.redraw() {
                Ok(()) => {}
                Err(RedrawError::TimeoutStreak) => {
                    anyhow::bail!(
                        "kms_surface_timeouts: {MAX_CONSECUTIVE_SURFACE_TIMEOUTS} consecutive \
                         acquire timeouts; exiting for a restart"
                    );
                }
                Err(RedrawError::Fatal(reason)) => {
                    anyhow::bail!("{reason}; exiting for a restart");
                }
            }
        }

        let plan = app.wake_plan(Instant::now());
        if plan.redraw_now {
            continue;
        }

        let loader = loader_open.then_some(&mut app.from_loader);
        let event = wake::next_wake_event(
            &plan,
            &app.cancel,
            control,
            &mut app.mat_pipeline.result_rx,
            loader,
        )
        .await;
        match event {
            WakeEvent::Cancelled => {
                info!("kms_viewer_cancelled");
                return Ok(());
            }
            WakeEvent::Command(cmd) => app.handle_control_command(cmd),
            WakeEvent::ControlClosed => {
                info!("kms_viewer_control_closed");
                return Ok(());
            }
            WakeEvent::MatResult(result) => app.accept_mat_result(result),
            WakeEvent::MatClosed => {
                anyhow::bail!("all matting workers exited; no photo can be prepared")
            }
            WakeEvent::Photo(PhotoLoaded { prepared, priority }) => {
                // queue_for_wake (next process_tick) submits deferred
                // photos before reading the loader again.
                app.deferred_images.push_back(QueuedImage {
                    image: prepared,
                    priority,
                });
            }
            WakeEvent::LoaderClosed => {
                debug!("kms_viewer_loader_closed");
                loader_open = false;
            }
            WakeEvent::DwellElapsed => {}
        }
    }
}
