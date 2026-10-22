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
//! * **Pacing law (transitions): render ahead, clock by frame index.** The
//!   loop renders transition frames back to back with no wait of its own;
//!   the FIFO swapchain acquire blocks when every image is either on screen
//!   or queued, so the loop runs at most the swapchain depth (two frames)
//!   ahead of the display, exactly like `frametest --drm`. Frame k of a
//!   transition is meant for vblank `anchor + k`, where the anchor is the
//!   vblank after the most recent one when the first frame is rendered (a
//!   non-blocking `DRM_IOCTL_WAIT_VBLANK`, relative 0), and it is drawn for
//!   that vblank's time, `anchor_time + k·T` with T the mode's exact period
//!   (`pacing::FrameClock`). Under FIFO, frames that are not late flip on
//!   consecutive vblanks, so frame k is shown exactly when its clock says.
//!   A frame rendered after the time it was meant for (the display is
//!   already behind) is drawn for the moment it is rendered instead
//!   (`pacing::frame_time`), so a GPU that cannot keep up gives a transition
//!   of the right length with repeats, not a slow-motion one. A KMS
//!   transition ends when its frame for progress 1 has been rendered
//!   (`TransitionState::is_complete`).
//! * **Lead-in.** The V3D clock idles at its lowest step through a dwell
//!   and needs 150-200 ms of load to reach full speed, which made every
//!   transition's first frames late. So for the last `TRANSITION_LEAD_IN`
//!   (300 ms) of a dwell whose next photo is ready, the loop renders the
//!   resting photo on every refresh (`wake::plan_wake`, `lead_in_frame`):
//!   the same picture, so nothing visible changes. It stops two refreshes
//!   before the dwell ends, so the frames it queued have flipped when the
//!   transition's first frame anchors the clock; and a plan asked after the
//!   dwell ended (a lead-in redraw blocks on the swapchain and can carry
//!   the clock past it) ticks at once, which starts the transition. These are not transition
//!   frames: the transition clock still anchors at the transition's first
//!   frame and the audit and metric are unchanged. The lead-in only runs on
//!   a visible slideshow: asleep (K2), with the panel dark, or under the
//!   Wi-Fi overlay the slideshow is paused (`wake.visible()` is false), so
//!   there is no lead-in and no frame; a dwell that resumes picks the
//!   lead-in up again if it is still due.
//! * **Why not wait for the vblank before each frame** (the v1.2.0-rc1 law).
//!   Gate 1 on the Pi measured it: every transition alternated 16.7 and
//!   33.3 ms frames and none reached 60, while `frametest --drm` (render
//!   ahead) held 60 on the same build. Waiting for vblank V and only then
//!   starting frame k leaves frame k less than one refresh for CPU work,
//!   the transition pass, the 4K upscale and Mesa's display copy before its
//!   flip at V+1; with the loop and GPU in series around the present, a
//!   frame of slightly over one refresh costs two, and the next frame cannot
//!   start before the vblank after that. Rendering ahead starts frame k+1
//!   as soon as an image is free, so a frame over budget loses only its
//!   excess and the queue absorbs single slow frames
//!   (`pacing::tests::with_the_loop_in_series_with_the_gpu_render_ahead_loses_only_the_excess`).
//! * **Late frames** (`pacing::FlipAudit`). Neither the loop's timing nor
//!   the frame clock can show a frame the GPU finished after the vblank it
//!   was meant for. So each transition frame registers a submitted-work-done
//!   callback, and while a transition is audited a flip-observer thread
//!   (`FlipObserver`) waits on every vblank on its own dup of the card fd
//!   and polls the device; the audit replays FIFO (a frame flips at the
//!   first vblank by which it is complete, never before its predecessor) to
//!   find the vblank each frame actually showed on.
//!   `transition_frame_metric` then reports what the panel showed: frames,
//!   `avg_fps`, best and worst time a frame stayed on screen, and
//!   `late_frames`, the refreshes on which no new frame appeared. The audit
//!   is closed after the transition's last frame (the loop waits, at most a
//!   few refreshes, until every frame has flipped); with `late_frames=0`
//!   the panel showed every frame on consecutive refreshes. Each frame's
//!   flip and the bounds on its GPU time are logged at debug level
//!   (`kms_frame`), for T4. With debug on, and where the device offers
//!   timestamp queries, the line also carries the GPU time of the frame's
//!   draw and upscale passes (`gpu_draw_ms`, `gpu_upscale_ms`; `PassTimer`
//!   in viewer.rs), read back off the render path by the observer's poll.
//! * **Observer failures.** The observer's blocking vblank waits run off the
//!   render path, so they never stall a frame. A failed wait (disabled CRTC:
//!   immediate EINVAL; vblanks stopped: the kernel's 3 s timeout) ends that
//!   transition's audit (its metric line has no `late_frames`); two in a
//!   row stop auditing for the process (`pacing::PacerHealth`). The observer
//!   is idle (parked, no vblank waits) outside transitions and while dark.
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
//!   So: while dark the loop presents nothing and nothing waits on a vblank
//!   (a display-power command first abandons any transition audit, which
//!   parks the flip observer); before the off commit the loop lets every
//!   submitted frame reach the screen (GPU idle, then two vblanks: with
//!   render-ahead at most two frames are queued behind the one on screen),
//!   so no flip is queued behind the commit; and the wake commit happens
//!   before the next present.
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

    let mut app = App::new(
        from_loader,
        to_manager_displayed,
        cancel,
        cfg,
        decode_scaling,
    );
    app.enter_greeting();
    app.output = Some(Output::Kms {
        size,
        refresh_period: target.refresh_period,
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

    // The observer's own dup of the card fd and its own device handle; it
    // is joined in kms_teardown before the device and the card go.
    let observer_display = target.display_handle()?;
    let device = app
        .gpu
        .as_ref()
        .map(|gpu| gpu.device.clone())
        .ok_or_else(|| anyhow::anyhow!("no GPU device after install"))?;
    let mut pacer = FramePacer::new(display, FlipObserver::spawn(observer_display, device));
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

/// The one safe teardown order (crate::drm_display, rule 2): the flip
/// observer thread (joined; it holds a device handle), then the swapchain
/// and every GPU object go first (dropping `App` drops the surface, the
/// device and the overlay scenes' device handles), then the instance (where
/// Mesa's display WSI joins the vblank wait thread that polls the card fd),
/// and only then the card fd, which also releases DRM master and lets the
/// kernel restore the console. Each step is logged so the journal shows the
/// order on every stop.
fn kms_teardown(app: App, pacer: FramePacer, instance: wgpu::Instance, target: KmsTarget) {
    // First the flip observer: it holds a device handle (keeping the device
    // alive past the app would break the order below) and a dup'd card fd.
    info!("kms_teardown: stopping the flip observer");
    drop(pacer);
    info!("kms_teardown: releasing swapchain and GPU objects");
    drop(app);
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

    /// Ask for a frame of whatever is on screen (the lead-in).
    fn request_kms_redraw(&self) {
        if let Some(Output::Kms {
            redraw_requested, ..
        }) = &self.output
        {
            redraw_requested.store(true, Ordering::Relaxed);
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

/// The display handle the loop uses (power, connector checks, the anchor
/// query) and the per-transition frame clock; see "Pacing law".
struct FramePacer {
    display: KmsDisplay,
    observer: FlipObserver,
    /// The transition being rendered: its clock and the next frame index.
    transition: Option<TransitionPacing>,
    /// Pass times of submitted frames not yet logged, by target vblank.
    pass_times: VecDeque<(u64, PassTimesCell)>,
}

struct TransitionPacing {
    clock: pacing::FrameClock,
    next_index: u32,
}

impl FramePacer {
    fn new(display: KmsDisplay, observer: FlipObserver) -> Self {
        Self {
            display,
            observer,
            transition: None,
            pass_times: VecDeque::new(),
        }
    }

    /// The first frame of a transition is about to be rendered: anchor its
    /// clock on the vblank after the most recent one (a non-blocking query)
    /// and start the audit.
    fn open_transition(&mut self) {
        let period = self.display.refresh_period();
        let last = match self.display.last_vblank() {
            Ok(vblank) => Some(self.observer.extend(vblank)),
            Err(err) => {
                debug!(error = %err, "kms_vblank_query_failed; transition clock from now, not audited");
                None
            }
        };
        let clock = match last {
            Some(last) => pacing::FrameClock::after_vblank(last, period),
            None => pacing::FrameClock::from_now(Instant::now(), period),
        };
        self.observer.begin(last.is_some());
        self.pass_times.clear();
        self.transition = Some(TransitionPacing {
            clock,
            next_index: 0,
        });
    }

    /// The clock for the next frame of the open transition.
    fn next_frame(&self) -> Option<PacedFrame> {
        self.transition.as_ref().map(|t| PacedFrame {
            anchor: t.clock.anchor_time(),
            index_time: t.clock.index_time(t.next_index),
            time_passes: tracing::enabled!(Level::DEBUG),
        })
    }

    /// A transition frame was presented (its completion flag and pass
    /// times, if the GPU work was submitted).
    fn presented(&mut self, submission: Option<PacedSubmission>) {
        let Some(t) = self.transition.as_mut() else {
            return;
        };
        if let (Some(submission), Some(target)) = (submission, t.clock.target(t.next_index)) {
            self.observer.submitted(target, submission.done);
            if let Some(passes) = submission.passes {
                self.pass_times.push_back((target, passes));
            }
        }
        t.next_index += 1;
    }

    /// Log the frames whose flip is now known, each with its pass times.
    fn log_resolved(&mut self) {
        for frame in self.observer.take_resolved() {
            while self
                .pass_times
                .front()
                .is_some_and(|(target, _)| *target < frame.target)
            {
                self.pass_times.pop_front();
            }
            let passes = match self.pass_times.front() {
                Some((target, cell)) if *target == frame.target => {
                    let passes = cell.get().copied();
                    self.pass_times.pop_front();
                    passes
                }
                _ => None,
            };
            log_resolved_frame(frame, passes);
        }
    }

    /// The transition's last frame has been rendered: wait (at most a few
    /// refreshes) until the audit knows every frame's flip and report what
    /// the panel showed.
    fn close_transition(&mut self, app: &mut App) {
        if self.transition.take().is_none() {
            return;
        }
        let period = self.display.refresh_period();
        let summary = self.observer.finish(period * CLOSE_AUDIT_REFRESHES);
        self.log_resolved();
        self.pass_times.clear();
        app.set_kms_display_cadence(
            summary.and_then(|summary| DisplayCadence::from_audit(summary, period)),
        );
    }

    /// Stop timing and auditing the open transition without a result (a
    /// display-power change interrupts it).
    fn abandon_transition(&mut self, app: &mut App) {
        if self.transition.take().is_some() {
            self.observer.discard();
            self.pass_times.clear();
            app.set_kms_display_cadence(None);
        }
    }
}

/// How long closing an audit may wait for the last frames to flip: with
/// render-ahead at most two are queued behind the one flipping, so this is
/// slack for a frame the GPU finishes late, not a time the loop normally
/// spends.
const CLOSE_AUDIT_REFRESHES: u32 = 8;

/// One `kms_frame` line. `gpu_draw_ms` and `gpu_upscale_ms` are measured
/// (timestamp queries; `-` when not available or, for the upscale, when the
/// frame was drawn natively); the `gpu_ms_at_*` pair bounds the whole
/// submission from vblank-granular observations.
fn log_resolved_frame(frame: pacing::ResolvedFrame, passes: Option<PassTimes>) {
    let ms = |d: Duration| format!("{:.2}", d.as_secs_f64() * 1000.0);
    let dash = || "-".to_string();
    debug!(
        target_vblank = frame.target,
        flip_vblank = frame.flip,
        late_refreshes = frame.flip.saturating_sub(frame.target),
        gpu_ms_at_least = format_args!("{:.1}", frame.gpu_at_least.as_secs_f32() * 1000.0),
        gpu_ms_at_most = ?frame.gpu_at_most.map(|d| format!("{:.1}", d.as_secs_f32() * 1000.0)),
        gpu_draw_ms = %passes.map_or_else(dash, |p| ms(p.draw)),
        gpu_upscale_ms = %passes.and_then(|p| p.upscale).map_or_else(dash, ms),
        "kms_frame"
    );
}

/// Watches every vblank while a transition is audited, off the render path:
/// a thread that blocks on the next vblank (its own dup of the card fd),
/// polls the device so the frames' completion callbacks run, and feeds the
/// observation to the shared `pacing::FlipAudit`. Parked outside
/// transitions. See "Late frames" in the module docs.
struct FlipObserver {
    shared: Arc<ObserverShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct ObserverShared {
    state: std::sync::Mutex<ObserverState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct ObserverState {
    /// Observing (a transition is being audited).
    active: bool,
    quit: bool,
    audit: pacing::FlipAudit,
    /// Every observation of the audited transition succeeded.
    complete: bool,
    counter: pacing::VblankCounter,
    /// The most recent vblank seen (sequence, time).
    last: Option<(u64, Instant)>,
    resolved: Vec<pacing::ResolvedFrame>,
    health: pacing::PacerHealth,
}

impl FlipObserver {
    fn spawn(mut display: KmsDisplay, device: wgpu::Device) -> Self {
        let shared = Arc::new(ObserverShared {
            state: std::sync::Mutex::new(ObserverState::default()),
            changed: std::sync::Condvar::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("kms-flip-observer".into())
            .spawn(move || {
                let shared = thread_shared;
                loop {
                    {
                        let mut state = shared.state.lock().expect("observer lock");
                        while !state.active && !state.quit {
                            state = shared.changed.wait(state).expect("observer lock");
                        }
                        if state.quit {
                            return;
                        }
                    }
                    let waited = display.wait_next_vblank();
                    let _ = device.poll(wgpu::PollType::Poll);
                    let now = Instant::now();
                    let mut state = shared.state.lock().expect("observer lock");
                    match waited {
                        Ok(vblank) => {
                            state.health.succeeded();
                            let sequence = state.counter.extend(vblank.sequence);
                            state.last = Some((sequence, monotonic_to_instant(vblank.monotonic)));
                            if state.active {
                                let resolved = state.audit.observe(sequence, now);
                                state.resolved.extend(resolved);
                            }
                        }
                        Err(err) => {
                            let off_for_process = state.health.failed();
                            state.complete = false;
                            state.active = false;
                            if off_for_process {
                                warn!(error = %err, "kms_vblank_wait_failed; transitions are no longer audited in this process");
                            } else {
                                warn!(error = %err, "kms_vblank_wait_failed; this transition is not audited");
                            }
                        }
                    }
                    shared.changed.notify_all();
                }
            })
            .expect("spawn the flip observer thread");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ObserverState> {
        self.shared.state.lock().expect("observer lock")
    }

    /// Extend a vblank reading to 64 bits and place it on the `Instant`
    /// clock (shared counter with the observer thread).
    fn extend(&self, vblank: drm_display::Vblank) -> (u64, Instant) {
        let mut state = self.lock();
        let reading = (
            state.counter.extend(vblank.sequence),
            monotonic_to_instant(vblank.monotonic),
        );
        state.last = Some(reading);
        reading
    }

    /// Start auditing a transition (when its clock could be anchored on a
    /// vblank and auditing is not off for the process).
    fn begin(&self, anchored: bool) {
        let mut state = self.lock();
        state.audit.discard();
        state.resolved.clear();
        state.health.transition_ended();
        state.complete = anchored && state.health.may_wait();
        state.active = state.complete;
        drop(state);
        self.shared.changed.notify_all();
    }

    fn submitted(&self, target: u64, done: Arc<AtomicBool>) {
        let mut state = self.lock();
        if !state.active {
            return;
        }
        // The most recent vblank the observer or the anchor query saw: a
        // frame cannot complete before the vblank after it.
        let after = state.last.map_or(target.saturating_sub(1), |(seq, _)| seq);
        state.audit.submitted(target, after, done, Instant::now());
    }

    /// Wait until every audited frame has flipped (or `limit` passes), then
    /// close the audit. `None` when it was incomplete.
    fn finish(&self, limit: Duration) -> Option<pacing::AuditSummary> {
        let deadline = Instant::now() + limit;
        let mut state = self.lock();
        while state.active && state.audit.has_unresolved() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .expect("observer lock")
                .0;
        }
        let complete = state.complete && state.active;
        let last = state.last.map_or(0, |(seq, _)| seq);
        let (summary, resolved) = state.audit.finish(last, Instant::now());
        state.resolved.extend(resolved);
        state.active = false;
        complete.then_some(summary)
    }

    fn discard(&self) {
        let mut state = self.lock();
        state.audit.discard();
        state.active = false;
        state.complete = false;
    }

    fn take_resolved(&self) -> Vec<pacing::ResolvedFrame> {
        std::mem::take(&mut self.lock().resolved)
    }
}

impl Drop for FlipObserver {
    fn drop(&mut self) {
        {
            let mut state = self.lock();
            state.quit = true;
            state.active = false;
        }
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            // At most one vblank wait (or the kernel's 3 s timeout if
            // vblanks have stopped) before the thread sees `quit`.
            let _ = thread.join();
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
    /// The open transition has a frame rendered (its clock is anchored).
    fn transition_anchored(&self) -> bool {
        self.mode()
            .wake()
            .transition_state()
            .is_some_and(TransitionState::is_anchored)
    }

    /// KMS: what the panel showed during the transition being measured;
    /// `None` when the audit was incomplete.
    fn set_kms_display_cadence(&mut self, display: Option<DisplayCadence>) {
        if let Some(metric) = self.transition_frame_metric.as_mut() {
            metric.display = display;
        }
    }
}

/// A control command on KMS. A display-power change first abandons the
/// transition audit (nothing waits on a vblank around the power commit).
fn handle_command(app: &mut App, pacer: &mut FramePacer, cmd: ViewerCommand) -> KmsCommand {
    if matches!(cmd, ViewerCommand::DisplayPower { .. }) {
        pacer.abandon_transition(app);
    }
    app.handle_kms_command(&mut pacer.display, cmd)
}

async fn drive(
    app: &mut App,
    control: &mut Receiver<ViewerCommand>,
    pacer: &mut FramePacer,
) -> anyhow::Result<()> {
    let mut loader_open = true;
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
                Ok(cmd) => match handle_command(app, pacer, cmd) {
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
        // Transition frames render ahead, bounded by the FIFO acquire, on
        // the frame clock of "Pacing law".
        let power = *app.presentation.power();
        if power.may_present() && app.take_kms_redraw_request() {
            let animating = app.transition_animating();
            let starting = animating && !app.transition_anchored();
            if pacer.transition.is_some() && (!animating || starting) {
                // The previous transition's last frame is rendered.
                pacer.close_transition(app);
            }
            if animating && pacer.transition.is_none() {
                // A new transition, or one resumed after a display-power
                // change abandoned its clock (it then continues from wall
                // time: its frames are drawn for when they are rendered).
                pacer.open_transition();
            }
            let paced = if animating { pacer.next_frame() } else { None };
            match app.redraw(paced) {
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
            if paced.is_some() {
                // Only a presented frame takes a vblank of the clock.
                if let Some(submission) = app.paced_submission.take() {
                    pacer.presented(Some(submission));
                }
            }
            pacer.log_resolved();
        }

        let plan = app.wake_plan(Instant::now());
        if plan.lead_in_frame {
            // The lead-in: one more frame of the resting photo, so the GPU
            // clock has ramped up when the transition starts (wake.rs).
            app.request_kms_redraw();
        }
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
            WakeEvent::Command(cmd) => match handle_command(app, pacer, cmd) {
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
