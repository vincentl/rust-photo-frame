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
//!   frame. When nothing needs drawing the loop sleeps in
//!   `wake::next_wake_event` until a control command, a matting result, a
//!   decoded photo (only while the preload queue is short), or the dwell
//!   deadline. See `wake.rs`.
//! * **Pacing model (transitions).** Each transition frame is rendered right
//!   after a vblank, which the loop waits for explicitly
//!   (`DRM_IOCTL_WAIT_VBLANK` on the connector's CRTC, see `VblankPacer`),
//!   and is drawn for that vblank's kernel timestamp. The transition's clock
//!   starts at the vblank before its first frame (`TransitionState::
//!   anchor_once`), so the first frame shows progress 0 and each later frame
//!   advances by exactly the time between the vblanks the two frames are
//!   paced on, which is the time between the vblanks they are shown on.
//!   `transition_frame_metric` records the same clock, so it reports the
//!   display's cadence. Why not let FIFO's blocking acquire pace on its own:
//!   after a dwell every swapchain image but the one on screen is idle, so
//!   the first frames of a transition are handed out back to back and
//!   presented a few ms apart, ahead of the display, and the transition
//!   clock (and the metric) would run ahead of what the panel shows. With
//!   the explicit wait, at most one frame is queued and an idle image is
//!   always free, so the acquire no longer decides the timing; FIFO still
//!   guarantees no tearing. Frames outside a transition (greeting, sleep,
//!   the first frame of a photo) render at once. If the vblank wait fails,
//!   the rest of that transition falls back to FIFO pacing (logged).
//! * **Runtime display changes.** Every idle wake checks that the connector
//!   is still connected and still offers the mode in use
//!   (`KmsDisplay::check_unchanged`); if not, the process exits with an
//!   error and `Restart=always` brings it back, and the restart waits for a
//!   connected display and re-enumerates its mode. A lost or outdated
//!   swapchain, any other surface error, or a streak of acquire timeouts
//!   also ends the process the same way. Changes are not handled in place
//!   in v1.2. Losing DRM master at runtime would need logind or VT
//!   switching, which this stack does not use; "not master" is a startup
//!   failure (greetd or plymouth still holding the display), retried by
//!   systemd until its StartLimit and then left failed.
//! * **Shutdown.** Every exit (SIGTERM/SIGINT/stdin via the cancel token,
//!   the control or matting channel closing, a fatal surface error) returns
//!   through `run_kms`, which tears down in the only safe order: GPU objects
//!   (swapchain) → `Instance` → card. See `kms_teardown`.

use super::wake::{self, WakeEvent};
use super::*;
use crate::drm_display::{self, KmsDisplay, KmsRequest, KmsTarget, ModeRequest};

/// `PHOTOFRAME_DRM_CONNECTOR`, e.g. `HDMI-A-1`; default: first connected.
const CONNECTOR_ENV: &str = "PHOTOFRAME_DRM_CONNECTOR";
/// `PHOTOFRAME_DRM_MODE`, `WxH[@Hz]`: an explicit mode, which must exist.
/// Default: [`KIOSK_MODE`] when the panel offers it, else its EDID-preferred
/// mode.
const MODE_ENV: &str = "PHOTOFRAME_DRM_MODE";
/// The mode the sway kiosk config pins on HDMI-A-1/2
/// (setup/system/modules/50-greetd.sh), so both stacks drive the panel the
/// same way by default. TVs often list 1080p60 or 4K30 as their preferred
/// mode even when they accept 4K60.
const KIOSK_MODE: ModeRequest = ModeRequest {
    width: 3840,
    height: 2160,
    refresh_hz: Some(60),
};
/// How often to look for a display while none is connected at startup.
/// One cheap KMS query per second; a panel that appears is picked up
/// within a second.
const DISPLAY_POLL: Duration = Duration::from_secs(1);

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
        prefer_mode: Some(KIOSK_MODE),
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
    // 1. Card first, so this process (not v3dv's own open) holds master. A
    //    panel that is not connected yet (boot before the TV, receiver in
    //    standby) is waited for instead of exhausting the unit's StartLimit.
    let mut first_attempt = true;
    let target = drm_display::wait_for_display(
        || {
            // Log the enumeration once, not on every poll.
            let mut lines = Vec::new();
            let result =
                drm_display::open_target(&request, &mut |line| lines.push(line.to_string()));
            if first_attempt || result.is_ok() {
                for line in &lines {
                    info!("kms_probe: {line}");
                }
            }
            first_attempt = false;
            result
        },
        DISPLAY_POLL,
        &cancel,
        {
            let mut announced = false;
            move |err| {
                if !announced {
                    warn!(error = %err, "kms_waiting_for_display: polling every second");
                    announced = true;
                }
            }
        },
    )
    .await?;
    let Some(target) = target else {
        info!("kms_viewer_cancelled before a display appeared");
        return Ok(());
    };
    info!(target = %target.describe(), "kms_target");
    target.assert_master()?;
    // A dup of the card fd for vblank waits and connector checks; dropping
    // it never releases master (the original fd in `target` stays open).
    let display = target.display_handle()?;
    // 2. Only now the wgpu instance.
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
    let mut trace = |line: &str| info!("kms_probe: {line}");
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

    let mut pacer = VblankPacer::new(display);
    let result = drive(&mut app, &mut control, &mut pacer).await;
    if let Err(err) = &result {
        warn!(error = %err, "kms_viewer_exiting_with_error");
    }
    kms_teardown(app, pacer, instance, target);
    result
}

/// The one safe teardown order (crate::drm_display, rule 2): the swapchain
/// and every GPU object go first (dropping `App` drops the surface, the
/// device and the overlay scenes' device handles), then the instance (where
/// Mesa's display WSI joins the vblank wait thread that polls the card fd),
/// and only then the card fd, which also releases DRM master and lets the
/// kernel restore the console. Each step is logged so the journal shows the
/// order on every stop.
fn kms_teardown(app: App, pacer: VblankPacer, instance: wgpu::Instance, target: KmsTarget) {
    info!("kms_teardown: releasing swapchain and GPU objects");
    drop(app);
    // The dup'd fd; any position before `target` is safe.
    drop(pacer);
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

    /// The slideshow is showing a transition (the frames the pacer times).
    fn transition_animating(&self) -> bool {
        self.mode_kind() == ViewerModeKind::Wake && self.mode().wake().transition_state().is_some()
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

/// Times transition frames on the display's vblanks; see "Pacing model"
/// in the module docs.
struct VblankPacer {
    display: KmsDisplay,
    /// Cleared when a vblank wait fails; the rest of that transition is
    /// paced by FIFO alone. Re-armed once no transition is animating.
    enabled: bool,
}

impl VblankPacer {
    fn new(display: KmsDisplay) -> Self {
        Self {
            display,
            enabled: true,
        }
    }

    /// Wait for the next vblank and return its time as an `Instant`, the
    /// clock the next transition frame is drawn for. `None` when vblank
    /// pacing is unavailable for this transition.
    fn next_frame_time(&mut self) -> Option<Instant> {
        if !self.enabled {
            return None;
        }
        match self.display.wait_next_vblank() {
            Ok(vblank) => Some(monotonic_to_instant(vblank.monotonic)),
            Err(err) => {
                warn!(error = %err, "kms_vblank_wait_failed; FIFO pacing for the rest of this transition");
                self.enabled = false;
                None
            }
        }
    }

    fn rearm(&mut self) {
        self.enabled = true;
    }
}

/// Convert a kernel CLOCK_MONOTONIC timestamp (vblank times are monotonic)
/// to an `Instant`, which is CLOCK_MONOTONIC on Linux too: subtract its age
/// from `Instant::now()`. The two clock reads are adjacent, so the error is
/// a few hundred nanoseconds.
fn monotonic_to_instant(timestamp: Duration) -> Instant {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let instant_now = Instant::now();
    let monotonic_now = Duration::new(now.tv_sec as u64, now.tv_nsec as u32);
    instant_now
        .checked_sub(monotonic_now.saturating_sub(timestamp))
        .unwrap_or(instant_now)
}

async fn drive(
    app: &mut App,
    control: &mut Receiver<ViewerCommand>,
    pacer: &mut VblankPacer,
) -> anyhow::Result<()> {
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

        // Every requested redraw presents on KMS (the surface is always
        // configured and the overlays never have a zero size), except after
        // an acquire timeout, which re-arms the request itself.
        if app.take_kms_redraw_request() {
            let frame_time = if app.transition_animating() {
                pacer.next_frame_time()
            } else {
                pacer.rearm();
                None
            };
            match app.redraw(frame_time) {
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
        if plan.redraw_now || plan.tick_now {
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

        // Runtime display changes: the loop only waits while idle, so every
        // wait ends here. An unplugged connector or a panel that no longer
        // offers the mode ends this process; the restart waits for a
        // display and picks its mode again.
        if let Err(err) = pacer.display.check_unchanged() {
            anyhow::bail!("kms_display_changed: {err}; exiting for a restart");
        }
    }
}
