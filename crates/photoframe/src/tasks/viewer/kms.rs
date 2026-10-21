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
//!   `transition_frame_metric` records the same clock, so its frame times
//!   report the vblanks the loop paced on. Why not let FIFO's blocking
//!   acquire pace on its own: after a dwell every swapchain image but the one
//!   on screen is idle, so the first frames of a transition are handed out
//!   back to back and presented a few ms apart, ahead of the display, and the
//!   transition clock (and the metric) would run ahead of what the panel
//!   shows. With the explicit wait, at most one frame is queued and an idle
//!   image is always free, so the acquire no longer decides the timing; FIFO
//!   still guarantees no tearing. Frames outside a transition (greeting,
//!   sleep, the resting photo) render at once, except the first frame after
//!   a transition, which is paced too so the audit below can close.
//! * **Late frames.** Vblank-to-vblank times cannot show a frame whose GPU
//!   work finishes after the vblank it was meant for: the CPU loop stays on
//!   time while the panel repeats a frame. So each paced frame registers a
//!   submitted-work-done callback, every pacing wake polls the device, and
//!   `pacing::FlipAudit` replays FIFO (a frame flips at the first vblank by
//!   which it is complete, never before its predecessor) to count the
//!   refreshes on which no new frame appeared. `transition_frame_metric`
//!   logs that count as `late_frames`; with best ≈ worst ≈ one refresh and
//!   `late_frames=0` the panel showed every frame on consecutive refreshes.
//! * **GPU slack.** The explicit wait gives a frame less than one refresh
//!   from its vblank to finish, where FIFO's run-ahead allowed up to two.
//!   That is accepted: (1) the exact clock is the point, and FIFO's slack
//!   came with the run-ahead that put every transition's first frames ahead
//!   of the display; (2) the trade is bounded, because after a miss one frame
//!   stays queued and every later frame of that transition gets two refreshes
//!   of GPU time, so a miss costs exactly one repeated refresh and does not
//!   cascade; (3) it is measured, not assumed: `late_frames` counts every
//!   such repeat, which is what Pi gate 1 and the T4 native-resolution
//!   decision read.
//! * **Pacer failures.** A failed vblank wait (disabled CRTC: immediate
//!   EINVAL; vblanks stopped: the kernel's 3 s timeout) leaves the rest of
//!   that transition to FIFO alone and drops its `late_frames` field; two
//!   failures in a row disable vblank pacing for the process
//!   (`pacing::PacerHealth`), so a stopped CRTC costs at most two 3 s stalls.
//! * **Runtime display changes.** Every idle wake checks that the connector
//!   is still connected and still offers the mode in use
//!   (`KmsDisplay::check_unchanged`; the dwell-deadline wake, once per
//!   photo, forces a probe so a panel swapped between two checks is seen
//!   too); if not, the process exits with an
//!   error and `Restart=always` brings it back, and the restart waits for a
//!   connected display and re-enumerates its mode. A lost or outdated
//!   swapchain, any other surface error, or a streak of acquire timeouts
//!   also ends the process the same way. Changes are not handled in place
//!   in v1.2. Losing DRM master at runtime would need logind or VT
//!   switching, which this stack does not use; "not master" is a startup
//!   failure (greetd or plymouth still holding the display), retried by
//!   systemd until its StartLimit and then left failed.
//! * **Display power** (`display-power` on the control socket, which
//!   `powerctl` and buttond use). The viewer switches the panel with its
//!   own blocking atomic commits on the CRTC (`KmsDisplay::set_active`):
//!   off commits `ACTIVE = 0`, on commits a fresh `MODE_ID`, the
//!   connector's `CRTC_ID` and `ACTIVE = 1` with `ALLOW_MODESET`, the same
//!   full modeset Mesa does when it first takes the display, so a wake
//!   drives the HDMI link up exactly as startup does (vc4 does not reliably
//!   recover from a mere power toggle, per picframe3). `PanelPower` changes
//!   only when the commit succeeded; the journal logs the kernel's `ACTIVE`
//!   read back and, on wake, a vblank received from the running CRTC.
//!   **Rule: Mesa never presents onto a dark CRTC.** Mesa does not know the
//!   viewer switched the CRTC off. From 25.3 its display WSI presents with a
//!   plain atomic flip, which the kernel rejects on an inactive CRTC
//!   ("requesting event but off"), and Mesa then loses the swapchain (a
//!   restart for us; wgpu hides the error at present). Up to 25.2 it falls
//!   back to `drmModeSetCrtc` and lights the panel behind the viewer's back.
//!   So: while dark the loop presents nothing and waits on no vblank (a
//!   vblank wait on a dark CRTC fails at once and two failures disable
//!   pacing); before the off commit the loop lets every submitted frame
//!   reach the screen (GPU idle, then two vblanks), so no flip is queued
//!   behind the commit; and the wake commit happens before the next present.
//!   The connector check is skipped while dark and for
//!   `CONNECTOR_SETTLE_AFTER_WAKE` after a wake: a sleeping sink may drop
//!   hot-plug detect and take seconds to raise it again. State machine:
//!   `presentation::PanelPower`.
//!   **A wake never leaves the panel dark with nothing left to try**
//!   (`presentation::wake_or_restart`). When the connector reads connected,
//!   the mode the CRTC kept must still be on its list (a sink or EDID can
//!   change while dark), a failed commit is retried once, and otherwise the
//!   process exits: the restart picks a mode again and lights the panel with
//!   Mesa's startup modeset. A restart while the panel is dark (a deploy, a
//!   crash) starts in `display_wait`, which answers `display-power` while no
//!   display reads connected: a wake forces a connector probe, and starts
//!   on the display if that finds it.
//!   What this does not prove (gate 2 checks by eye): `ACTIVE=1` re-runs
//!   vc4's full CRTC and encoder enable, which is close to what an atomic
//!   "DPMS on" does. At 4K60 the link also needs HDMI 2.0 scrambling set
//!   over SCDC during that enable; a sink whose DDC is still asleep relies
//!   on vc4's hot-plug `reset_link` when hot-plug detect returns. The
//!   `ACTIVE` read-back and the vblank show the pixel pipeline runs, not
//!   that the TV shows a picture.
//!   **Before Mesa's first present** (a boot or restart in sleep hours,
//!   `presentation::power_commit`): an off switches off whatever the
//!   console left lit (nothing, if the console never drove the connector),
//!   and a wake commits nothing. Mesa starts with the connector inactive,
//!   so its first present is its own full modeset: no console framebuffer
//!   on the way, one HDMI retrain. An off received while waiting for a
//!   display is remembered, so the viewer starts dark (`display_wait`).
//!   One cosmetic edge is left as it is: Mesa's flip-event thread starts
//!   only when an acquire finds no idle image. Until then a frame queued
//!   before the off can stay booked as flipping, so after the wake an older
//!   frame may show for a moment until the image pool cycles.
//!   A wake that fails twice restarts the viewer, whose startup state is
//!   the schedule's (asleep in sleep hours). buttond treats the failed
//!   power-on like a failed power-off: it retries with backoff, and once
//!   the panel is on it sends `set-state awake` again, which the restarted
//!   viewer applies after its startup state (reviews D3, M2).
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
/// How long after a wake's modeset the connector check waits: a sink
/// that dropped hot-plug detect while asleep may take seconds to raise it
/// again, and until then "asleep" and "unplugged" look the same. Pi gate 2
/// logs the actual time (`kms_power_on: connector reads connected`).
const CONNECTOR_SETTLE_AFTER_WAKE: Duration = Duration::from_secs(15);
/// Stop waiting for that log line after this long.
const CONNECTOR_REPORT_LIMIT: Duration = Duration::from_secs(60);

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
        force_probe: false,
    })
}

/// Run the viewer on DRM/KMS until cancelled or a fatal display error.
pub async fn run_kms(
    from_loader: PhotoReceiver,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
    cfg: Arc<crate::config::Configuration>,
    mut control: Receiver<ViewerCommand>,
    decode_scaling: crate::tasks::loader::DecodeScaling,
) -> anyhow::Result<()> {
    // Declaration order is the reverse of teardown order: if anything below
    // unwinds, Rust drops `app` (swapchain) before `instance` before
    // `target` (card fd), which is the order kms_teardown enforces on the
    // normal path.
    let request = kms_request_from_env()?;
    // 1. Card first, so this process (not v3dv's own open) holds master. A
    //    panel that is not connected yet (boot before the TV, receiver in
    //    standby) is waited for instead of exhausting the unit's StartLimit.
    //    While waiting, display-power is answered (`display_wait.rs`): a
    //    wake forces a probe, which may find a TV that dropped hot-plug
    //    detect in standby; other commands are held and applied below.
    let mut first_attempt = true;
    let waited = super::display_wait::wait_answering(
        |force_probe| {
            // Log the enumeration once, not on every poll.
            let mut lines = Vec::new();
            let request = KmsRequest {
                force_probe,
                ..request.clone()
            };
            let result =
                drm_display::open_target(&request, &mut |line| lines.push(line.to_string()));
            if first_attempt || force_probe || result.is_ok() {
                for line in &lines {
                    info!("kms_probe: {line}");
                }
            }
            first_attempt = false;
            result
        },
        |target: &KmsTarget| {
            (
                target.connector_name.clone(),
                format!("{}x{}@{}", target.width, target.height, target.vrefresh_hz),
            )
        },
        request.connector.as_deref(),
        DISPLAY_POLL,
        &cancel,
        &mut control,
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
    let (target, held, start_dark) = match waited {
        super::display_wait::Waited::Ready {
            target,
            held,
            start_dark,
        } => (target, held, start_dark),
        super::display_wait::Waited::Cancelled => {
            info!("kms_viewer_cancelled before a display appeared");
            return Ok(());
        }
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

    let mut app = App::new(from_loader, to_manager_displayed, cancel, cfg, decode_scaling);
    app.enter_greeting();
    app.output = Some(Output::Kms {
        size,
        redraw_requested: Arc::new(AtomicBool::new(false)),
    });
    app.install_gpu(parts, size, "kms");
    app.configure_fixed_surface(size);
    // Commands that arrived while waiting for the display, in order.
    for cmd in held {
        app.handle_control_command(cmd);
    }
    info!(
        width = size.width,
        height = size.height,
        "viewer presenting straight to KMS"
    );

    let mut pacer = VblankPacer::new(display);
    if start_dark {
        // The last power request while waiting was off (review D1): start
        // dark, before anything is presented.
        let (reply, _) = app.kms_display_power(
            &mut pacer.display,
            Some(config_model::control::PowerState::Off),
            None,
        );
        info!(ok = reply.ok, detail = ?reply.detail, "kms_power_off: starting dark, as last requested while waiting");
    }
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

    /// A control command on KMS: display power needs the display; the rest
    /// is `handle_control_command`.
    fn handle_kms_command(&mut self, display: &mut KmsDisplay, cmd: ViewerCommand) -> KmsCommand {
        match cmd {
            ViewerCommand::DisplayPower {
                state,
                output,
                reply,
            } => {
                let was_dark = self.presentation.power().is_dark();
                let (answer, restart) = self.kms_display_power(display, state, output.as_deref());
                let _ = reply.send(answer);
                if let Some(reason) = restart {
                    KmsCommand::Restart(reason)
                } else if was_dark && !self.presentation.power().is_dark() {
                    KmsCommand::Lit
                } else {
                    KmsCommand::Done
                }
            }
            cmd => {
                self.handle_control_command(cmd);
                KmsCommand::Done
            }
        }
    }

    /// `display-power`: see "Display power" in the module docs. The second
    /// value, when set, ends the process for a restart (a wake that cannot
    /// be committed; systemd's restart then lights the panel with the
    /// startup modeset).
    fn kms_display_power(
        &mut self,
        display: &mut KmsDisplay,
        state: Option<config_model::control::PowerState>,
        output: Option<&str>,
    ) -> (config_model::control::Reply, Option<String>) {
        use config_model::control::{ErrorKind, PowerState, Reply};
        let connector = display.connector_name().to_string();
        if let Some(output) = output
            && output != "*"
            && !output.eq_ignore_ascii_case(&connector)
        {
            return (
                Reply::error(
                    ErrorKind::NotDriven,
                    format!("this viewer drives {connector}, not {output}"),
                ),
                None,
            );
        }
        use crate::tasks::viewer::presentation::{PowerCommit, power_commit};
        let dark = self.presentation.power().is_dark();
        let mesa_has_presented = self.frames_presented > 0;
        match state {
            Some(PowerState::Off) if !dark && !mesa_has_presented && !display.crtc_routed() => {
                // Nothing was ever lit on this connector (the console never
                // drove it, Mesa has not presented): nothing to switch off.
                self.presentation.power_mut().set_dark();
                info!(%connector, "kms_power_off: panel off (nothing was lit yet)");
            }
            Some(PowerState::Off) if !dark => {
                // Every frame already submitted reaches the screen first, so
                // Mesa has no flip queued behind the off commit.
                self.drain_presents(display);
                if let Err(err) = display.set_active(false) {
                    warn!(error = %err, "kms_power_off_failed");
                    return (Reply::error(ErrorKind::Failed, err.to_string()), None);
                }
                self.presentation.power_mut().set_dark();
                let active = display.crtc_active();
                info!(%connector, crtc_active = ?active, "kms_power_off: panel off");
            }
            Some(PowerState::On)
                if dark
                    && power_commit(true, mesa_has_presented) == PowerCommit::MesaFirstPresent =>
            {
                // Mesa's first present is its own full modeset (review N3):
                // nothing to commit, no console flash, one HDMI retrain.
                self.presentation.power_mut().set_lit(Instant::now());
                info!(%connector, "kms_power_on: Mesa's first present does the modeset");
            }
            Some(PowerState::On) if dark => {
                // The wake rule (`presentation::wake_or_restart`): check the
                // mode the CRTC kept against the connector's list when it
                // reads connected, retry a failed commit once, and otherwise
                // restart, so a wake never leaves the panel dark with
                // nothing left to try.
                let mode_check = if display.is_connected() {
                    display.check_unchanged(false)
                } else {
                    Ok(())
                };
                let woken = crate::tasks::viewer::presentation::wake_or_restart(
                    mode_check,
                    || display.set_active(true),
                    |err| {
                        warn!(error = %err, "kms_power_on_failed: retrying once");
                        std::thread::sleep(Duration::from_millis(200));
                    },
                );
                if let Err(failure) = woken {
                    use crate::tasks::viewer::presentation::WakeFailure;
                    let (what, err) = match failure {
                        WakeFailure::ModeGone(err) => ("the kept mode is gone", err),
                        WakeFailure::CommitFailed(err) => ("the wake commit failed twice", err),
                    };
                    let reason = format!(
                        "kms_power_on: {what} ({err}); exiting for a restart, whose startup modeset lights the panel"
                    );
                    warn!("{reason}");
                    return (
                        Reply::error(
                            ErrorKind::Failed,
                            format!("{what} ({err}); the viewer restarts to light the panel"),
                        ),
                        Some(reason),
                    );
                }
                self.presentation.power_mut().set_lit(Instant::now());
                // The kernel's view after the modeset: ACTIVE, and a vblank
                // from the CRTC, which only a running CRTC delivers.
                let active = display.crtc_active();
                let vblank = display.wait_next_vblank().map(|_| ());
                info!(
                    %connector,
                    crtc_active = ?active,
                    vblank = ?vblank,
                    "kms_power_on: modeset committed"
                );
            }
            _ => {}
        }
        // Pause or resume the slideshow and ask for a frame of what is now
        // on screen.
        self.sync_presentation();
        (
            Reply::power(
                self.presentation.power().reported(),
                connector,
                display.mode_label(),
            ),
            None,
        )
    }

    /// Wait until every frame submitted so far has been scanned out: the
    /// GPU work is done, then two vblanks pass (the flip in progress lands
    /// on the first, a frame queued behind it on the second). Nothing is
    /// presented in between: commands are handled between frames.
    fn drain_presents(&self, display: &mut KmsDisplay) {
        if let Some(gpu) = self.gpu.as_ref() {
            let _ = gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_millis(500)),
            });
        }
        for _ in 0..2 {
            if display.wait_next_vblank().is_err() {
                break;
            }
        }
    }

    /// One matting result from the awaited channel; same bookkeeping as
    /// `drain_mat_results`.
    fn accept_mat_result(&mut self, result: MatResult) {
        super::accept_mat_result(&mut self.mat_inflight, &mut self.ready_results, result);
    }
}

/// What a control command did, for the loop.
enum KmsCommand {
    Done,
    /// A dark panel was lit (start the hot-plug report).
    Lit,
    /// Exit for a restart, with the reason.
    Restart(String),
}

/// Times transition frames on the display's vblanks; see "Pacing model"
/// and "Pacer failures" in the module docs.
struct VblankPacer {
    display: KmsDisplay,
    health: pacing::PacerHealth,
    counter: pacing::VblankCounter,
}

impl VblankPacer {
    fn new(display: KmsDisplay) -> Self {
        Self {
            display,
            health: pacing::PacerHealth::default(),
            counter: pacing::VblankCounter::default(),
        }
    }

    /// Wait for the next vblank; return its time on the `Instant` clock (the
    /// clock the next transition frame is drawn for) and its sequence number.
    /// `None` when vblank pacing is unavailable.
    fn wait(&mut self) -> Option<(Instant, u64)> {
        if !self.health.may_wait() {
            return None;
        }
        match self.display.wait_next_vblank() {
            Ok(vblank) => {
                self.health.succeeded();
                Some((
                    monotonic_to_instant(vblank.monotonic),
                    self.counter.extend(vblank.sequence),
                ))
            }
            Err(err) => {
                if self.health.failed() {
                    warn!(
                        error = %err,
                        failures = pacing::PACER_FAILURES_TO_DISABLE,
                        "kms_vblank_wait_failed; vblank pacing disabled for this process, FIFO alone paces transitions"
                    );
                } else {
                    warn!(error = %err, "kms_vblank_wait_failed; FIFO pacing for the rest of this transition");
                }
                None
            }
        }
    }
}

/// Place a kernel CLOCK_MONOTONIC timestamp (vblank times are monotonic) on
/// the `Instant` clock, which is CLOCK_MONOTONIC on Linux too; see
/// `pacing::place_on_instant`. The two clock reads are adjacent, so the
/// error is a few hundred nanoseconds.
fn monotonic_to_instant(timestamp: Duration) -> Instant {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let instant_now = Instant::now();
    let monotonic_now = Duration::new(now.tv_sec as u64, now.tv_nsec as u32);
    pacing::place_on_instant(instant_now, monotonic_now, timestamp)
}

impl App {
    /// Run the device's completion callbacks (the late-frame audit's input).
    fn poll_device(&self) {
        if let Some(gpu) = self.gpu.as_ref() {
            let _ = gpu.device.poll(wgpu::PollType::Poll);
        }
    }

    /// KMS: the audited late-frame count for the transition being measured;
    /// `None` when vblank pacing was unavailable for part of it.
    fn set_kms_late_frames(&mut self, late_frames: Option<u32>) {
        if let Some(metric) = self.transition_frame_metric.as_mut() {
            metric.late_frames = late_frames;
        }
    }
}

async fn drive(
    app: &mut App,
    control: &mut Receiver<ViewerCommand>,
    pacer: &mut VblankPacer,
) -> anyhow::Result<()> {
    let mut loader_open = true;
    let mut audit = pacing::FlipAudit::default();
    // Every frame of the transition being audited was vblank-paced.
    let mut audit_complete = true;
    // After a wake: when its modeset committed, until the connector
    // first reads connected again (logged for Pi gate 2).
    let mut wake_hpd_since: Option<Instant> = None;
    loop {
        if app.cancel.is_cancelled() {
            info!("kms_viewer_cancelled");
            return Ok(());
        }
        // Commands that arrived while a frame was rendering (at most one
        // refresh of delay).
        loop {
            match control.try_recv() {
                Ok(cmd) => match app.handle_kms_command(&mut pacer.display, cmd) {
                    KmsCommand::Done => {}
                    KmsCommand::Lit => wake_hpd_since = Some(Instant::now()),
                    KmsCommand::Restart(reason) => anyhow::bail!("{reason}"),
                },
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
        // an acquire timeout, which re-arms the request itself. While the
        // panel is dark nothing presents and the request stays armed.
        let power = *app.presentation.power();
        if power.may_present() && app.take_kms_redraw_request() {
            let animating = app.transition_animating();
            // Transition frames, and the first frame after a transition
            // (it closes the audit), wait for the next vblank.
            let paced = if animating || audit.has_unresolved() {
                pacer.wait()
            } else {
                None
            };
            if let Some((_, vblank)) = paced {
                app.poll_device();
                if animating {
                    audit.observe(vblank);
                } else {
                    let late = audit.finish(vblank);
                    app.set_kms_late_frames(audit_complete.then_some(late));
                }
            } else if animating {
                audit_complete = false;
            } else if audit.has_unresolved() {
                let _ = audit.finish(0);
                app.set_kms_late_frames(None);
            }
            if !animating {
                pacer.health.transition_ended();
                audit_complete = true;
            }
            let frame_time = if animating {
                paced.map(|(t, _)| t)
            } else {
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
            if let Some(done) = app.paced_submission.take()
                && let Some((_, vblank)) = paced
                && animating
            {
                audit.submitted(vblank, done);
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
        let dwell_elapsed = matches!(event, WakeEvent::DwellElapsed);
        match event {
            WakeEvent::Cancelled => {
                info!("kms_viewer_cancelled");
                return Ok(());
            }
            WakeEvent::Command(cmd) => match app.handle_kms_command(&mut pacer.display, cmd) {
                KmsCommand::Done => {}
                KmsCommand::Lit => wake_hpd_since = Some(Instant::now()),
                KmsCommand::Restart(reason) => anyhow::bail!("{reason}"),
            },
            WakeEvent::ControlClosed => {
                info!("kms_viewer_control_closed");
                return Ok(());
            }
            WakeEvent::MatResult(result) => app.accept_mat_result(result),
            WakeEvent::MatClosed => {
                anyhow::bail!("all matting workers exited; no photo can be prepared")
            }
            WakeEvent::Photo(PhotoLoaded {
                prepared,
                priority,
                partner,
            }) => {
                // queue_for_wake (next process_tick) submits deferred
                // photos before reading the loader again.
                app.deferred_images.push_back(QueuedImage {
                    image: prepared,
                    priority,
                    partner,
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
        // display and picks its mode again. The dwell deadline (once per
        // photo, just before the next transition) forces a probe; other
        // wakes read the kernel's cached state. Not while the panel is dark
        // or settling after a wake (see "Display power").
        let now = Instant::now();
        if let Some(since) = wake_hpd_since {
            if pacer.display.is_connected() {
                info!(
                    after_ms = now.saturating_duration_since(since).as_millis() as u64,
                    "kms_power_on: connector reads connected"
                );
                wake_hpd_since = None;
            } else if now.saturating_duration_since(since) > CONNECTOR_REPORT_LIMIT {
                warn!("kms_power_on: connector still not connected a minute after the wake");
                wake_hpd_since = None;
            }
        }
        if app
            .presentation
            .power()
            .may_check_connector(now, CONNECTOR_SETTLE_AFTER_WAKE)
            && let Err(err) = pacer.display.check_unchanged(dwell_elapsed)
        {
            anyhow::bail!("kms_display_changed: {err}; exiting for a restart");
        }
    }
}
