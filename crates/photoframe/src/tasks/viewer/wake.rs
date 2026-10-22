//! When the viewer has to run, as a pure function of its state, and the one
//! place it waits when it does not.
//!
//! The viewer has exactly two reasons to do work:
//!
//! * **Redraw now**: a scene requested a frame, or a transition is
//!   animating. The loop renders frame after frame; on DRM/KMS each
//!   transition frame waits for the next vblank first (see `kms.rs`), so
//!   the display paces it.
//! * **Tick now**: a transition just finished, so the uploads and matting
//!   refill it held back (they compete with the animation for memory
//!   bandwidth) run right away instead of on the next transition's frames.
//! * **An event**: a control command, a matting result, a decoded photo
//!   when the preload queue is short, or the dwell deadline of the photo
//!   on screen. Nothing else changes what is on screen, so between these
//!   the viewer sleeps: zero frames and zero wakeups while a photo rests.
//!
//! The DRM/KMS loop (`kms.rs`) drives itself from [`plan_wake`] and
//! [`next_wake_event`]. The Wayland driver ([`drive_forwarding`]) waits on
//! the same plan: it owns the control, matting-result and loader channels,
//! forwards what arrives to the winit thread as user events, and ticks the
//! winit thread fast only while [`WakePlan::tick_fast`], so a resting photo
//! costs no wakeups there either.

use super::MatResult;
use crate::events::{PhotoLoaded, ViewerCommand};
use crate::tasks::photo_channel::PhotoReceiver;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc::Receiver, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// The viewer state [`plan_wake`] depends on.
#[derive(Clone, Copy, Debug)]
pub(super) struct WakeInputs {
    /// The slideshow (Wake scene) is active; not Greeting, not Sleep.
    pub(super) slideshow_active: bool,
    /// The viewer is asleep; it neither prepares nor shows photos.
    pub(super) sleeping: bool,
    /// A scene asked for a frame that has not been rendered yet.
    pub(super) redraw_requested: bool,
    pub(super) transition_active: bool,
    /// A transition finalized since the last plan (taken once).
    pub(super) transition_just_finished: bool,
    /// When the current photo went on screen; `None` before the first one.
    pub(super) displayed_at: Option<Instant>,
    pub(super) dwell: Duration,
    /// The surface has a usable size (always, on KMS).
    pub(super) surface_ready: bool,
    /// Screens uploaded and waiting their turn (one canvas each).
    pub(super) pending: usize,
    /// Decoded photos submitted to the matting workers (a pair is two).
    pub(super) in_flight: usize,
    /// Decoded photos taken from the loader but not yet submitted.
    pub(super) deferred: usize,
    /// Photos of the first deferred screen (0 when none waits).
    pub(super) deferred_head: usize,
    /// Photos of the largest screen the loader can send: 2 with portrait
    /// pairing, else 1.
    pub(super) largest_screen: usize,
    /// `viewer-preload-count`.
    pub(super) preload: usize,
    /// When the Wi-Fi overlay's lease runs out, if one is held (see
    /// `presentation.rs`). It is a deadline in every scene: with the
    /// slideshow paused under the overlay there is no dwell to wake for.
    pub(super) overlay_expires_at: Option<Instant>,
    /// Wayland only: how often to look at the window's size while the
    /// surface is ready and the viewer is awake. Sway can change the output
    /// mode or scale without winit reporting it, and each control step
    /// re-reads the size, so a bounded poll replaces the unbounded 4 ms one
    /// (at most one wakeup per period). `None` on KMS, whose mode never
    /// changes at runtime.
    pub(super) surface_poll: Option<Duration>,
    /// Wayland only: the Greeting or Sleep overlay asked for a frame that
    /// has not been presented (a failed present leaves the request set).
    /// KMS re-arms its own requests.
    pub(super) overlay_pending: bool,
    /// The next photo is ready, so the transition starts when the dwell ends
    /// (an uploaded screen is waiting, or one is staged).
    pub(super) photo_staged: bool,
    /// DRM/KMS: how long before a transition the GPU is given real work, so
    /// its clock has ramped up by the first transition frame
    /// (`TRANSITION_LEAD_IN` in viewer.rs). Zero on Wayland.
    pub(super) lead_in: Duration,
}

/// How soon a pending overlay redraw is retried. Slow enough to cost nothing,
/// fast enough that repeated acquire timeouts still reach the viewer's
/// recovery (`MAX_CONSECUTIVE_SURFACE_TIMEOUTS`) in seconds.
const OVERLAY_RETRY: Duration = Duration::from_millis(250);

/// How soon a deferred photo that could not be submitted (the matting queue
/// was full) is tried again when no matting result is due to wake the loop.
const REFILL_RETRY: Duration = Duration::from_millis(250);

/// What the loop does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WakePlan {
    /// Render again without waiting.
    pub(super) redraw_now: bool,
    /// Run the control step again without waiting. One-shot (its input is
    /// taken), so it cannot keep the loop awake.
    pub(super) tick_now: bool,
    /// Wayland: keep ticking the control step on the fast cadence
    /// (`CONTROL_TICK_INTERVAL`) because something advances with each tick
    /// rather than on an event: a transition is animating, or the surface
    /// handshake is still counting its repeated size reports. False while a
    /// photo rests, so the wait is for events and the dwell deadline only.
    /// KMS renders back to back (`redraw_now`) and has no handshake.
    pub(super) tick_fast: bool,
    /// Wake at this instant even if no event arrives: the dwell of the photo
    /// on screen ends. Always in the future when set, so a wait on it can
    /// never spin.
    pub(super) deadline: Option<Instant>,
    /// Take the next decoded photo from the loader when one arrives. This is
    /// exactly `MattingBridge::queue_for_wake`'s own gate for reading the
    /// loader channel, so the channel's backpressure is unchanged: while it
    /// is false the photo stays queued in the loader.
    pub(super) want_photo: bool,
    /// DRM/KMS: the dwell ends within `lead_in` and the next photo is ready:
    /// render the resting photo again (the same picture) on every refresh
    /// until the transition starts. `redraw_now` is set too.
    pub(super) lead_in_frame: bool,
}

/// **The decoded-photo bound (review M4).** The viewer's preload counts
/// decoded photos, not screens: a canvas waiting its turn counts one, a
/// decoded photo in matting or waiting for it counts one, so a portrait pair
/// counts two. A screen of `photos` photos is admitted (read from the loader,
/// or submitted to matting) only if it fits beside the `used` ones:
/// `used + photos ≤ preload`, or the stage is idle, so a pair still flows at
/// `viewer-preload-count: 1`. For a single photo this is `used < preload`,
/// the bound before pairing, so with pairing off nothing changes; with it on
/// the viewer never holds more decoded photos than with it off. The loader is
/// read only when the largest screen it can send would fit, because a
/// message's size is known only once it is taken.
pub(super) fn screen_fits(used: usize, photos: usize, preload: usize) -> bool {
    used == 0 || used + photos <= preload
}

pub(super) fn plan_wake(inputs: WakeInputs, now: Instant) -> WakePlan {
    // A transition only animates on the slideshow. One left behind by a
    // switch to Sleep must not keep the loop rendering frames nobody draws.
    let animating = inputs.slideshow_active && inputs.transition_active;
    let redraw_now = inputs.redraw_requested || animating;

    // The dwell clock only matters on the slideshow with a photo resting on
    // screen. Once it has run out, starting the transition needs a staged
    // photo, which arrives as an event (matting result or loader); until
    // then there is nothing to wait for but events.
    //
    // The lead-in (KMS): with the next photo ready, the last `lead_in` of the
    // dwell renders the resting photo on every refresh, so the GPU's clock
    // (which idles at its lowest step through a dwell) has ramped up by the
    // transition's first frame. The loop wakes at the start of the lead-in
    // instead of at the end of the dwell.
    let mut lead_in_frame = false;
    let mut deadline = match inputs.displayed_at {
        Some(shown) if inputs.slideshow_active && !inputs.transition_active => {
            let dwell_end = shown + inputs.dwell;
            let lead_in = if inputs.photo_staged {
                inputs.lead_in
            } else {
                Duration::ZERO
            };
            let lead_in_start = dwell_end.checked_sub(lead_in).unwrap_or(dwell_end);
            if !lead_in.is_zero() && now >= lead_in_start && now < dwell_end {
                lead_in_frame = true;
            }
            Some(if lead_in_frame {
                dwell_end
            } else {
                lead_in_start
            })
            .filter(|deadline| *deadline > now)
        }
        _ => None,
    };
    let redraw_now = redraw_now || lead_in_frame;

    // Bounded polls and retries. Each is "wake no later than": the loop still
    // sleeps on events, and none of them applies to the other's state.
    let mut sooner = |at: Instant| deadline = Some(deadline.map_or(at, |d| d.min(at)));
    if inputs.surface_ready
        && !inputs.sleeping
        && let Some(period) = inputs.surface_poll
    {
        sooner(now + period);
    }
    if !inputs.slideshow_active && inputs.overlay_pending {
        sooner(now + OVERLAY_RETRY);
    }
    // A photo waiting to be submitted that no step has taken: with the queue
    // short and nothing animating, the step that ran last should have
    // submitted it, so its submit failed (queue full). If no matting result
    // is coming to wake the loop, ask again.
    if inputs.surface_ready
        && !inputs.sleeping
        && !inputs.transition_active
        && inputs.deferred_head > 0
        && screen_fits(
            inputs.pending + inputs.in_flight,
            inputs.deferred_head,
            inputs.preload,
        )
    {
        sooner(now + REFILL_RETRY);
    }
    // The Wi-Fi overlay's lease (presentation.rs): one that ends in the
    // future is a deadline in every scene (with the slideshow paused under
    // the overlay there is no dwell to wake for); one already over is taken
    // down by the next control step, so run it now.
    let overlay_due = inputs.overlay_expires_at.is_some_and(|at| at <= now);
    if let Some(at) = inputs.overlay_expires_at.filter(|at| *at > now) {
        sooner(at);
    }

    // The step that finalized a transition ran its uploads and matting
    // refill *before* the finalize, while both were still gated off, so the
    // deferred work runs on one more step right after it.
    let tick_now = (inputs.transition_just_finished && !inputs.sleeping) || overlay_due;

    // Exactly when MattingBridge::queue_for_wake reads the loader (the test
    // `want_photo_matches_the_bridge_gate` in viewer.rs holds the two
    // together): never while a transition animates; with a usable surface
    // it first submits every deferred photo and keeps reading while the
    // queue is short; without one it reads a single photo into `deferred`
    // and stops there.
    // Counted in photos, see `screen_fits`.
    let queued = inputs.pending + inputs.in_flight;
    let want_photo = !inputs.sleeping
        && !inputs.transition_active
        && if inputs.surface_ready {
            screen_fits(
                queued + inputs.deferred,
                inputs.largest_screen,
                inputs.preload,
            )
        } else {
            inputs.deferred == 0 && screen_fits(queued, inputs.largest_screen, inputs.preload)
        };

    // The surface-ready handshake needs two identical size reports 120 ms
    // apart, and each control step is a report (it polls the window's size),
    // so the steps must keep coming until the surface is ready.
    let tick_fast = redraw_now || !inputs.surface_ready;

    WakePlan {
        redraw_now,
        tick_now,
        tick_fast,
        deadline,
        want_photo,
        lead_in_frame,
    }
}

/// What woke the loop.
#[derive(Debug)]
pub(super) enum WakeEvent {
    Cancelled,
    Command(ViewerCommand),
    /// The control channel closed; only happens while the process shuts down.
    ControlClosed,
    MatResult(MatResult),
    /// Every matting worker has exited; no photo can be prepared again.
    MatClosed,
    Photo(PhotoLoaded),
    /// The loader channel closed. The caller stops passing it (`None`), so
    /// this is reported once.
    LoaderClosed,
    DwellElapsed,
}

/// Wait for the next event the plan cares about.
///
/// A closed channel is reported as its own event instead of being polled
/// again: a closed receiver is permanently ready, and selecting on it would
/// turn the wait into a spin.
pub(super) async fn next_wake_event(
    plan: &WakePlan,
    cancel: &CancellationToken,
    control: &mut Receiver<ViewerCommand>,
    mat_results: &mut Receiver<MatResult>,
    loader: Option<&mut PhotoReceiver>,
) -> WakeEvent {
    let take_photo = plan.want_photo && loader.is_some();
    let photo = async {
        match loader {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    };
    let dwell = async {
        match plan.deadline {
            Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => WakeEvent::Cancelled,
        cmd = control.recv() => match cmd {
            Some(cmd) => WakeEvent::Command(cmd),
            None => WakeEvent::ControlClosed,
        },
        result = mat_results.recv() => match result {
            Some(result) => WakeEvent::MatResult(result),
            None => WakeEvent::MatClosed,
        },
        photo = photo, if take_photo => match photo {
            Some(photo) => WakeEvent::Photo(photo),
            None => WakeEvent::LoaderClosed,
        },
        _ = dwell, if plan.deadline.is_some() => WakeEvent::DwellElapsed,
    }
}

/// What the winit thread publishes after every event batch: the plan for the
/// state it is now in, and how many of the driver's forwarded events it has
/// handled. The count is how the driver knows the plan describes a state
/// that includes everything it forwarded (see [`drive_forwarding`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct Published {
    pub(super) plan: WakePlan,
    pub(super) handled: u64,
}

impl Published {
    /// Before the winit thread has published anything.
    pub(super) fn idle() -> Self {
        Self {
            plan: WakePlan::IDLE,
            handled: 0,
        }
    }
}

impl WakePlan {
    /// Wait for events only.
    const IDLE: WakePlan = WakePlan {
        redraw_now: false,
        tick_now: false,
        tick_fast: false,
        deadline: None,
        want_photo: false,
        lead_in_frame: false,
    };
}

/// The winit thread's side of the end of an event batch, so the sequence in
/// [`end_of_batch`] can run (and be tested) without a window or GPU.
pub(super) trait BatchHost {
    /// One control step (`App::process_tick`).
    fn step(&mut self);
    /// The plan for the state now (`App::wake_plan`); takes the one-shot
    /// `tick_now` input.
    fn plan(&mut self, now: Instant) -> WakePlan;
}

/// Steps a finalized transition can owe after a batch. One in practice (the
/// refill that was gated off while it animated); the bound only guards a
/// loop that cannot happen, since finalizing again needs a new transition.
const MAX_OWED_STEPS: usize = 3;

/// End of a winit event batch (`about_to_wait`): run the step a window event
/// asked for, then the extra step a finalized transition owes (`tick_now`),
/// and return what to publish for the driver. Without the second step the
/// uploads and matting refill held back during the animation would wait for
/// the next event or dwell.
pub(super) fn end_of_batch(
    host: &mut impl BatchHost,
    step_wanted: bool,
    handled: u64,
    mut now: impl FnMut() -> Instant,
) -> Published {
    if step_wanted {
        host.step();
    }
    let mut plan = host.plan(now());
    for _ in 0..MAX_OWED_STEPS {
        if !plan.tick_now {
            break;
        }
        host.step();
        plan = host.plan(now());
    }
    Published { plan, handled }
}

/// What the Wayland driver hands to the winit thread.
#[derive(Debug)]
pub(super) enum Forward {
    /// Run the control step.
    Tick,
    Command(ViewerCommand),
    MatResult(MatResult),
    Photo(PhotoLoaded),
    /// Stop the event loop.
    Cancelled,
}

/// If forwarded events stay unhandled this long, send a tick: the winit
/// thread publishes after every batch it handles, so this only fires if a
/// publication was somehow missed, and it keeps that from freezing the
/// slideshow.
const UNHANDLED_RECHECK: Duration = Duration::from_secs(1);

/// The Wayland counterpart of the KMS loop's wait: owns the control,
/// matting-result and loader channels (the winit thread cannot await them),
/// sleeps in [`next_wake_event`] on the plan the winit thread last
/// published, and forwards each event to it.
///
/// The plan is trusted only while `published.handled` equals the number of
/// events forwarded. Between forwarding an event and the winit thread
/// handling it, the plan describes the state before the event: its deadline
/// may already have fired (re-sending it would spin) and its `want_photo`
/// would read the loader past the preload bound. Until the next publication
/// the driver therefore waits on events only.
///
/// The fast cadence is `tick_interval` after the previous tick, never
/// queued: a slow tick delays the next one instead of stacking behind it.
/// `forward` returns false when the winit thread is gone.
pub(super) async fn drive_forwarding(
    cancel: CancellationToken,
    mut control: Receiver<ViewerCommand>,
    mut mat_results: Receiver<MatResult>,
    loader: PhotoReceiver,
    mut published: watch::Receiver<Published>,
    tick_interval: Duration,
    mut forward: impl FnMut(Forward) -> bool,
) {
    let now = || tokio::time::Instant::now().into_std();
    let mut loader = Some(loader);
    let mut sent: u64 = 0;
    let mut last_forward = now();
    let mut last_tick = last_forward;

    // The winit thread starts its first control step as soon as it runs.
    if !forward(Forward::Tick) {
        return;
    }
    sent += 1;

    loop {
        let seen = *published.borrow_and_update();
        let plan = if seen.handled == sent {
            let mut plan = seen.plan;
            // `plan_wake` never publishes a deadline that has passed; if one
            // ever arrived, firing on it again after the tick it already
            // caused would spin, so a deadline at or before the last tick is
            // taken as served.
            plan.deadline = plan.deadline.filter(|deadline| *deadline > last_tick);
            if plan.tick_fast {
                let due = last_tick + tick_interval;
                plan.deadline = Some(plan.deadline.map_or(due, |d| d.min(due)));
            }
            plan
        } else {
            WakePlan {
                deadline: Some(last_forward + UNHANDLED_RECHECK),
                ..WakePlan::IDLE
            }
        };

        let event = tokio::select! {
            biased;
            changed = published.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
            event = next_wake_event(
                &plan,
                &cancel,
                &mut control,
                &mut mat_results,
                loader.as_mut(),
            ) => event,
        };
        let out = match event {
            WakeEvent::Cancelled => {
                let _ = forward(Forward::Cancelled);
                return;
            }
            WakeEvent::Command(cmd) => {
                debug!(command = ?cmd, "viewer_event_forward_command");
                Forward::Command(cmd)
            }
            WakeEvent::ControlClosed => {
                warn!("viewer control channel closed; stopping driver loop");
                return;
            }
            WakeEvent::MatResult(result) => Forward::MatResult(result),
            WakeEvent::MatClosed => {
                warn!("all matting workers exited; stopping the viewer");
                let _ = forward(Forward::Cancelled);
                return;
            }
            WakeEvent::Photo(photo) => Forward::Photo(photo),
            WakeEvent::LoaderClosed => {
                debug!("viewer_loader_closed");
                loader = None;
                continue;
            }
            WakeEvent::DwellElapsed => {
                last_tick = now();
                Forward::Tick
            }
        };
        if !forward(out) {
            warn!("viewer event proxy rejected an event; stopping driver loop");
            return;
        }
        sent += 1;
        last_forward = now();
    }
}

#[cfg(test)]
mod tests {
    use super::super::ImagePlane;
    use super::*;
    use crate::events::{PreparedImageCpu, ViewerState};
    use crate::tasks::photo_channel::PhotoSender;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    const DWELL: Duration = Duration::from_secs(8);

    /// A photo resting on the slideshow with a full preload queue.
    fn resting(now: Instant) -> WakeInputs {
        WakeInputs {
            slideshow_active: true,
            sleeping: false,
            redraw_requested: false,
            transition_active: false,
            transition_just_finished: false,
            displayed_at: Some(now),
            dwell: DWELL,
            surface_ready: true,
            pending: 3,
            in_flight: 0,
            deferred: 0,
            deferred_head: 0,
            largest_screen: 1,
            preload: 3,
            overlay_expires_at: None,
            surface_poll: None,
            overlay_pending: false,
            photo_staged: false,
            lead_in: Duration::ZERO,
        }
    }

    const LEAD_IN: Duration = Duration::from_millis(300);

    fn ready_for_lead_in(now: Instant) -> WakeInputs {
        WakeInputs {
            photo_staged: true,
            lead_in: LEAD_IN,
            ..resting(now)
        }
    }

    #[test]
    fn the_lead_in_starts_before_the_dwell_ends_when_the_next_photo_is_ready() {
        let shown = Instant::now();
        let inputs = ready_for_lead_in(shown);
        // Mid-dwell: sleep until the lead-in starts, not the dwell's end.
        let plan = plan_wake(inputs, shown + Duration::from_secs(1));
        assert!(!plan.lead_in_frame && !plan.redraw_now);
        assert_eq!(plan.deadline, Some(shown + DWELL - LEAD_IN));
        // In the lead-in: render every refresh until the dwell ends.
        let at = shown + DWELL - Duration::from_millis(100);
        let plan = plan_wake(inputs, at);
        assert!(plan.lead_in_frame && plan.redraw_now);
        assert_eq!(plan.deadline, Some(shown + DWELL));
        // At the dwell's end the transition starts (process_tick); no
        // lead-in past it.
        let plan = plan_wake(inputs, shown + DWELL);
        assert!(!plan.lead_in_frame);
    }

    #[test]
    fn no_lead_in_without_a_ready_photo_on_wayland_or_while_paused() {
        let shown = Instant::now();
        let at = shown + DWELL - Duration::from_millis(100);
        for inputs in [
            // Nothing to transition to: frames would be wasted.
            WakeInputs {
                photo_staged: false,
                ..ready_for_lead_in(shown)
            },
            // Wayland (lead_in zero).
            WakeInputs {
                lead_in: Duration::ZERO,
                ..ready_for_lead_in(shown)
            },
            // Paused: dark panel or Wi-Fi overlay (slideshow not visible),
            // or asleep.
            WakeInputs {
                slideshow_active: false,
                ..ready_for_lead_in(shown)
            },
            WakeInputs {
                slideshow_active: false,
                sleeping: true,
                ..ready_for_lead_in(shown)
            },
            // Already animating.
            WakeInputs {
                transition_active: true,
                ..ready_for_lead_in(shown)
            },
        ] {
            let plan = plan_wake(inputs, at);
            assert!(!plan.lead_in_frame, "{inputs:?}");
        }
    }

    #[test]
    fn resting_photo_sleeps_until_its_dwell_ends() {
        let now = Instant::now();
        let plan = plan_wake(resting(now), now);
        assert_eq!(
            plan,
            WakePlan {
                redraw_now: false,
                tick_now: false,
                tick_fast: false,
                deadline: Some(now + DWELL),
                want_photo: false,
                lead_in_frame: false,
            }
        );
    }

    /// R1: a pair screen at rest plans `displayed_at + dwell·m` (the dwell is
    /// the screen's, `WakeScene::dwell`), so the KMS loop wakes for it and the
    /// slideshow cannot stall on a pair.
    #[test]
    fn a_pair_at_rest_plans_its_longer_dwell() {
        use super::super::scenes::{pair_dwell_ms, screen_dwell};
        let now = Instant::now();
        let dwell_ms = DWELL.as_millis() as u64;
        let pair_ms = pair_dwell_ms(dwell_ms, 1.5);
        assert_eq!(pair_ms, 12_000);
        let pair = WakeInputs {
            dwell: screen_dwell(dwell_ms, pair_ms, true),
            ..resting(now)
        };
        assert_eq!(
            plan_wake(pair, now).deadline,
            Some(now + DWELL.mul_f32(1.5))
        );
        let solo = WakeInputs {
            dwell: screen_dwell(dwell_ms, pair_ms, false),
            ..resting(now)
        };
        assert_eq!(plan_wake(solo, now).deadline, Some(now + DWELL));
        // An empty scene (nothing current) rests the solo dwell.
        let scene = super::super::scenes::WakeScene::new(
            dwell_ms,
            pair_ms,
            crate::config::TransitionConfig::default(),
        );
        assert_eq!(scene.dwell(), DWELL);
    }

    #[test]
    fn an_overlay_lease_is_a_deadline_in_every_scene() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(5);
        // Paused slideshow under the overlay: the lease is the only deadline.
        let paused = WakeInputs {
            slideshow_active: false,
            overlay_expires_at: Some(soon),
            ..resting(now)
        };
        assert_eq!(plan_wake(paused, now).deadline, Some(soon));
        // Asleep with the overlay held: same.
        let asleep = WakeInputs {
            sleeping: true,
            ..paused
        };
        assert_eq!(plan_wake(asleep, now).deadline, Some(soon));
        // With a dwell running too, the earlier of the two.
        let both = WakeInputs {
            overlay_expires_at: Some(now + DWELL * 2),
            ..resting(now)
        };
        assert_eq!(plan_wake(both, now).deadline, Some(now + DWELL));
        let both = WakeInputs {
            overlay_expires_at: Some(soon),
            ..resting(now)
        };
        assert_eq!(plan_wake(both, now).deadline, Some(soon));
    }

    /// Review F3 under T5: a Wi-Fi overlay frame that was not presented is
    /// owed by the layer on screen, so the plan schedules a control step
    /// within OVERLAY_RETRY (that step runs `App::rearm_wifi_overlay`,
    /// which asks winit for the frame), without fast ticking. The covered
    /// greeting's stale request does not count (F9).
    #[test]
    fn an_owed_overlay_frame_gets_a_step_within_the_retry_period() {
        use super::super::ViewerModeKind;
        use super::super::presentation::{Layer, layer_owes_frame};
        let now = Instant::now();
        let covered = |overlay_owes| WakeInputs {
            slideshow_active: false,
            overlay_pending: layer_owes_frame(
                Layer::Overlay,
                |kind| kind == ViewerModeKind::Greeting,
                overlay_owes,
            ),
            ..resting(now)
        };
        let plan = plan_wake(covered(true), now);
        assert_eq!(plan.deadline, Some(now + OVERLAY_RETRY));
        assert!(!plan.tick_fast && !plan.redraw_now);
        let plan = plan_wake(covered(false), now);
        assert_eq!(plan.deadline, None, "nothing owed: sleep on events");
    }

    #[test]
    fn a_lapsed_overlay_lease_ticks_now_instead_of_waiting() {
        let now = Instant::now();
        for lapsed in [now, now - Duration::from_secs(1)] {
            let plan = plan_wake(
                WakeInputs {
                    slideshow_active: false,
                    overlay_expires_at: Some(lapsed),
                    ..resting(now)
                },
                now,
            );
            assert!(
                plan.tick_now,
                "a lapsed lease must not leave the loop asleep"
            );
            assert_eq!(plan.deadline, None, "and is never a deadline in the past");
        }
    }

    #[test]
    fn deadline_is_never_in_the_past() {
        let start = Instant::now();
        let inputs = resting(start);
        for later in [DWELL, DWELL + Duration::from_millis(1), DWELL * 3] {
            let plan = plan_wake(inputs, start + later);
            assert_eq!(plan.deadline, None, "at +{later:?}");
            assert!(!plan.redraw_now);
        }
        let plan = plan_wake(inputs, start + DWELL - Duration::from_millis(1));
        assert_eq!(plan.deadline, Some(start + DWELL));
    }

    #[test]
    fn transition_renders_back_to_back_without_a_deadline() {
        let now = Instant::now();
        let plan = plan_wake(
            WakeInputs {
                transition_active: true,
                pending: 0,
                ..resting(now)
            },
            now,
        );
        assert!(plan.redraw_now);
        assert_eq!(plan.deadline, None);
        // Preparation is deferred while a transition animates.
        assert!(!plan.want_photo);
    }

    #[test]
    fn a_redraw_request_renders_now_in_any_scene() {
        let now = Instant::now();
        for (slideshow_active, sleeping) in [(true, false), (false, false), (false, true)] {
            let plan = plan_wake(
                WakeInputs {
                    slideshow_active,
                    sleeping,
                    redraw_requested: true,
                    ..resting(now)
                },
                now,
            );
            assert!(plan.redraw_now);
        }
    }

    #[test]
    fn sleep_waits_for_events_only() {
        let now = Instant::now();
        let plan = plan_wake(
            WakeInputs {
                slideshow_active: false,
                sleeping: true,
                // Left behind by a switch to Sleep mid-transition.
                transition_active: true,
                pending: 0,
                ..resting(now)
            },
            now,
        );
        assert_eq!(
            plan,
            WakePlan {
                redraw_now: false,
                tick_now: false,
                tick_fast: false,
                deadline: None,
                want_photo: false,
                lead_in_frame: false,
            }
        );
    }

    #[test]
    fn a_finished_transition_runs_its_deferred_work_at_once() {
        let now = Instant::now();
        // The frame after finalize: no transition, photo just went on
        // screen, a result held back during the animation.
        let after = WakeInputs {
            transition_just_finished: true,
            pending: 1,
            in_flight: 0,
            ..resting(now)
        };
        let plan = plan_wake(after, now);
        assert!(plan.tick_now);
        assert!(!plan.redraw_now);
        // One-shot: the next plan (input taken) waits for the dwell.
        let plan = plan_wake(
            WakeInputs {
                transition_just_finished: false,
                ..after
            },
            now,
        );
        assert!(!plan.tick_now);
        assert_eq!(plan.deadline, Some(now + DWELL));
        // Asleep: deferred work waits for the wake command.
        let plan = plan_wake(
            WakeInputs {
                sleeping: true,
                slideshow_active: false,
                ..after
            },
            now,
        );
        assert!(!plan.tick_now);
    }

    #[test]
    fn greeting_preloads_but_has_no_dwell() {
        let now = Instant::now();
        let plan = plan_wake(
            WakeInputs {
                slideshow_active: false,
                displayed_at: None,
                pending: 0,
                ..resting(now)
            },
            now,
        );
        assert!(!plan.redraw_now);
        assert_eq!(plan.deadline, None);
        assert!(plan.want_photo);
    }

    fn photo(name: &str) -> PhotoLoaded {
        PhotoLoaded {
            prepared: PreparedImageCpu {
                path: name.into(),
                width: 1,
                height: 1,
                source_size: (1, 1),
                pixels: vec![0; 4],
            },
            priority: false,
            partner: None,
        }
    }

    fn mat_result(name: &str) -> MatResult {
        MatResult {
            path: name.into(),
            canvas: ImagePlane {
                width: 1,
                height: 1,
                pixels: vec![0; 4],
            },
            priority: false,
            partner: None,
            windows: Vec::new(),
            mat_kind: None,
            photos: 1,
        }
    }

    struct Sources {
        cancel: CancellationToken,
        control_tx: mpsc::Sender<ViewerCommand>,
        control: Receiver<ViewerCommand>,
        mat_tx: mpsc::Sender<MatResult>,
        mat: Receiver<MatResult>,
        loader_tx: PhotoSender,
        loader: PhotoReceiver,
    }

    fn sources() -> Sources {
        let (control_tx, control) = mpsc::channel(4);
        let (mat_tx, mat) = mpsc::channel(4);
        let (loader_tx, loader) = crate::tasks::photo_channel::photo_channel(4);
        Sources {
            cancel: CancellationToken::new(),
            control_tx,
            control,
            mat_tx,
            mat,
            loader_tx,
            loader,
        }
    }

    fn idle_plan() -> WakePlan {
        WakePlan {
            redraw_now: false,
            tick_now: false,
            tick_fast: false,
            deadline: None,
            want_photo: false,
            lead_in_frame: false,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dwell_deadline_wakes_exactly_at_the_deadline() {
        let mut s = sources();
        let start = tokio::time::Instant::now();
        let plan = WakePlan {
            deadline: Some(start.into_std() + DWELL),
            ..idle_plan()
        };
        let event = next_wake_event(
            &plan,
            &s.cancel,
            &mut s.control,
            &mut s.mat,
            Some(&mut s.loader),
        )
        .await;
        assert!(matches!(event, WakeEvent::DwellElapsed), "{event:?}");
        assert_eq!(start.elapsed(), DWELL);
    }

    #[tokio::test(start_paused = true)]
    async fn photo_stays_queued_in_the_loader_while_not_wanted() {
        let mut s = sources();
        s.loader_tx.send(photo("a.jpg")).await.unwrap();
        let start = tokio::time::Instant::now();
        let plan = WakePlan {
            deadline: Some(start.into_std() + Duration::from_secs(1)),
            ..idle_plan()
        };
        let event = next_wake_event(
            &plan,
            &s.cancel,
            &mut s.control,
            &mut s.mat,
            Some(&mut s.loader),
        )
        .await;
        assert!(matches!(event, WakeEvent::DwellElapsed), "{event:?}");
        // Backpressure held: the photo was not taken.
        assert_eq!(s.loader.len(), 1);

        let wanted = WakePlan {
            want_photo: true,
            ..idle_plan()
        };
        let event = next_wake_event(
            &wanted,
            &s.cancel,
            &mut s.control,
            &mut s.mat,
            Some(&mut s.loader),
        )
        .await;
        match event {
            WakeEvent::Photo(p) => assert_eq!(p.prepared.path, std::path::Path::new("a.jpg")),
            other => panic!("expected the photo, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn closed_loader_is_reported_once_then_ignored() {
        let mut s = sources();
        drop(s.loader_tx);
        let wanted = WakePlan {
            want_photo: true,
            ..idle_plan()
        };
        let event = next_wake_event(
            &wanted,
            &s.cancel,
            &mut s.control,
            &mut s.mat,
            Some(&mut s.loader),
        )
        .await;
        assert!(matches!(event, WakeEvent::LoaderClosed), "{event:?}");

        // The caller then passes no loader; the wait blocks on the other
        // sources instead of spinning on the closed channel.
        let start = tokio::time::Instant::now();
        let plan = WakePlan {
            deadline: Some(start.into_std() + DWELL),
            ..wanted
        };
        let event = next_wake_event(&plan, &s.cancel, &mut s.control, &mut s.mat, None).await;
        assert!(matches!(event, WakeEvent::DwellElapsed), "{event:?}");
        assert_eq!(start.elapsed(), DWELL);
    }

    #[tokio::test(start_paused = true)]
    async fn commands_and_matting_results_wake_the_loop() {
        let mut s = sources();
        s.control_tx
            .send(ViewerCommand::SetState(ViewerState::Asleep))
            .await
            .unwrap();
        let event =
            next_wake_event(&idle_plan(), &s.cancel, &mut s.control, &mut s.mat, None).await;
        assert!(
            matches!(
                event,
                WakeEvent::Command(ViewerCommand::SetState(ViewerState::Asleep))
            ),
            "{event:?}"
        );

        s.mat_tx.send(mat_result("b.jpg")).await.unwrap();
        let event =
            next_wake_event(&idle_plan(), &s.cancel, &mut s.control, &mut s.mat, None).await;
        assert!(matches!(event, WakeEvent::MatResult(_)), "{event:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn closed_control_and_matting_channels_are_events() {
        let mut s = sources();
        drop(s.mat_tx);
        let event =
            next_wake_event(&idle_plan(), &s.cancel, &mut s.control, &mut s.mat, None).await;
        assert!(matches!(event, WakeEvent::MatClosed), "{event:?}");

        let mut s = sources();
        drop(s.control_tx);
        let event =
            next_wake_event(&idle_plan(), &s.cancel, &mut s.control, &mut s.mat, None).await;
        assert!(matches!(event, WakeEvent::ControlClosed), "{event:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_wins_over_ready_work() {
        let mut s = sources();
        s.control_tx.send(ViewerCommand::ToggleState).await.unwrap();
        s.mat_tx.send(mat_result("c.jpg")).await.unwrap();
        s.cancel.cancel();
        let plan = WakePlan {
            deadline: Some(Instant::now()),
            want_photo: true,
            ..idle_plan()
        };
        let event = next_wake_event(
            &plan,
            &s.cancel,
            &mut s.control,
            &mut s.mat,
            Some(&mut s.loader),
        )
        .await;
        assert!(matches!(event, WakeEvent::Cancelled), "{event:?}");
    }

    #[test]
    fn fast_ticks_only_while_something_advances_per_tick() {
        let now = Instant::now();
        // A photo at rest, even with a full queue and a pending deadline.
        assert!(!plan_wake(resting(now), now).tick_fast);
        // A transition animating.
        let animating = WakeInputs {
            transition_active: true,
            ..resting(now)
        };
        assert!(plan_wake(animating, now).tick_fast);
        // A scene's own redraw request.
        let requested = WakeInputs {
            redraw_requested: true,
            ..resting(now)
        };
        assert!(plan_wake(requested, now).tick_fast);
        // The surface handshake polls the window size once per step.
        let handshake = WakeInputs {
            surface_ready: false,
            slideshow_active: false,
            displayed_at: None,
            pending: 0,
            ..resting(now)
        };
        assert!(plan_wake(handshake, now).tick_fast);
        // A transition left behind by a switch to Sleep animates nothing.
        let asleep = WakeInputs {
            slideshow_active: false,
            sleeping: true,
            transition_active: true,
            ..resting(now)
        };
        assert!(!plan_wake(asleep, now).tick_fast);
        // The one-shot post-finalize step is not a cadence.
        let finished = WakeInputs {
            transition_just_finished: true,
            ..resting(now)
        };
        let plan = plan_wake(finished, now);
        assert!(plan.tick_now && !plan.tick_fast);
    }

    /// A stand-in for the winit thread: handles each forwarded event, bumps
    /// the handled count and publishes the plan for the state it is in. The
    /// test steers that state with `set_plan`.
    struct Rig {
        events: Arc<Mutex<Vec<(Duration, &'static str)>>>,
        plan: Arc<Mutex<WakePlan>>,
        published: watch::Sender<Published>,
        handled: Arc<AtomicU64>,
        photo_over_read: Arc<AtomicBool>,
        start: tokio::time::Instant,
        control_tx: mpsc::Sender<ViewerCommand>,
        mat_tx: Option<mpsc::Sender<MatResult>>,
        loader_tx: PhotoSender,
    }

    const TICK: Duration = Duration::from_millis(4);

    /// Start the driver against a [`Rig`]. With `ack` false the stand-in
    /// never publishes (a winit thread that lost its publication).
    fn rig(ack: bool) -> Rig {
        let plan = Arc::new(Mutex::new(idle_plan()));
        let for_app = Arc::clone(&plan);
        rig_with(plan, move |_kind, handled| {
            ack.then(|| Published {
                plan: *for_app.lock().unwrap(),
                handled,
            })
        })
    }

    /// As [`rig`], but `app` decides what the winit thread publishes after
    /// handling each forwarded event (`None`: nothing).
    fn rig_with(
        plan: Arc<Mutex<WakePlan>>,
        mut app: impl FnMut(&'static str, u64) -> Option<Published> + Send + 'static,
    ) -> Rig {
        let start = tokio::time::Instant::now();
        let events = Arc::new(Mutex::new(Vec::new()));
        let handled = Arc::new(AtomicU64::new(0));
        let forwarded = Arc::new(AtomicU64::new(0));
        let photo_over_read = Arc::new(AtomicBool::new(false));
        let (published, published_rx) = watch::channel(Published::idle());
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<&'static str>();
        let (control_tx, control) = mpsc::channel(4);
        let (mat_tx, mat) = mpsc::channel(4);
        let (loader_tx, loader) = crate::tasks::photo_channel::photo_channel(4);

        // The winit thread.
        {
            let (events, handled, published) =
                (Arc::clone(&events), Arc::clone(&handled), published.clone());
            tokio::spawn(async move {
                while let Some(kind) = out_rx.recv().await {
                    events.lock().unwrap().push((start.elapsed(), kind));
                    // `handled` counts only events the app published for, as
                    // a silent winit thread never advances it.
                    if let Some(next) = app(kind, handled.load(Ordering::SeqCst) + 1) {
                        handled.store(next.handled, Ordering::SeqCst);
                        let _ = published.send(next);
                    }
                }
            });
        }
        // The driver.
        {
            let (handled, over_read) = (Arc::clone(&handled), Arc::clone(&photo_over_read));
            tokio::spawn(drive_forwarding(
                CancellationToken::new(),
                control,
                mat,
                loader,
                published_rx,
                TICK,
                move |forward| {
                    let before = forwarded.fetch_add(1, Ordering::SeqCst);
                    let kind = match forward {
                        Forward::Tick => "tick",
                        Forward::Command(_) => "command",
                        Forward::MatResult(_) => "mat",
                        Forward::Photo(_) => {
                            // A photo is read only when the plan includes
                            // every event forwarded before it.
                            if handled.load(Ordering::SeqCst) != before {
                                over_read.store(true, Ordering::SeqCst);
                            }
                            "photo"
                        }
                        Forward::Cancelled => "cancelled",
                    };
                    out_tx.send(kind).is_ok()
                },
            ));
        }
        Rig {
            events,
            plan,
            published,
            handled,
            photo_over_read,
            start,
            control_tx,
            mat_tx: Some(mat_tx),
            loader_tx,
        }
    }

    impl Rig {
        fn count(&self, kind: &str) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, k)| *k == kind)
                .count()
        }

        fn set_plan(&self, plan: WakePlan) {
            *self.plan.lock().unwrap() = plan;
            // The state the winit thread publishes at the end of a batch.
            let handled = self.handled.load(Ordering::SeqCst);
            self.published.send(Published { plan, handled }).unwrap();
        }
    }

    fn dwelling(rig: &Rig) -> WakePlan {
        WakePlan {
            deadline: Some(rig.start.into_std() + DWELL),
            ..idle_plan()
        }
    }

    fn animating() -> WakePlan {
        WakePlan {
            redraw_now: true,
            tick_fast: true,
            ..idle_plan()
        }
    }

    /// The point of T5: during an 8 s dwell with nothing pending, the driver
    /// wakes the winit thread once at the start and once at the deadline.
    /// (The 4 ms ticker sent 2000 in the same time.)
    #[tokio::test(start_paused = true)]
    async fn ticker_is_idle_during_a_dwell_with_no_pending_work() {
        let r = rig(true);
        tokio::task::yield_now().await;
        r.set_plan(dwelling(&r));
        tokio::time::sleep(DWELL - Duration::from_millis(1)).await;
        assert_eq!(r.count("tick"), 1, "{:?}", r.events.lock().unwrap());
        tokio::time::sleep(Duration::from_millis(2)).await;
        // The deadline: one tick, and no more until the state changes (the
        // rig's plan still has the now-past deadline, as the real one would
        // before the winit thread re-plans).
        r.set_plan(WakePlan {
            deadline: None,
            ..idle_plan()
        });
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(r.count("tick"), 2, "{:?}", r.events.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn ticker_resumes_while_a_transition_animates_then_goes_quiet() {
        let r = rig(true);
        tokio::task::yield_now().await;
        r.set_plan(dwelling(&r));
        tokio::time::sleep(DWELL + Duration::from_millis(1)).await;
        assert_eq!(r.count("tick"), 2);
        // The tick at the deadline started a transition.
        r.set_plan(animating());
        let before = r.count("tick");
        tokio::time::sleep(Duration::from_millis(400)).await;
        let during = r.count("tick") - before;
        assert!(
            (95..=101).contains(&during),
            "expected one tick per 4 ms for 400 ms, got {during}"
        );
        // The transition finalized: back to waiting for the next dwell.
        r.set_plan(WakePlan {
            deadline: Some(tokio::time::Instant::now().into_std() + DWELL),
            ..idle_plan()
        });
        let settled = r.count("tick");
        tokio::time::sleep(DWELL - Duration::from_millis(1)).await;
        assert_eq!(r.count("tick"), settled, "ticks while resting");
    }

    #[tokio::test(start_paused = true)]
    async fn ticker_resumes_on_a_matting_result_and_a_command() {
        let r = rig(true);
        tokio::task::yield_now().await;
        r.set_plan(dwelling(&r));
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(r.count("tick"), 1);
        let at = r.start.elapsed();
        r.mat_tx
            .as_ref()
            .unwrap()
            .send(mat_result("d.jpg"))
            .await
            .unwrap();
        r.control_tx.send(ViewerCommand::ToggleState).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        {
            let events = r.events.lock().unwrap();
            let kinds: Vec<_> = events.iter().map(|(_, k)| *k).collect();
            assert_eq!(kinds, ["tick", "command", "mat"], "{events:?}");
            // Delivered the instant they arrived, not on a later tick. (The
            // control channel is polled first, as in the KMS loop.)
            assert!(events[1..].iter().all(|(t, _)| *t == at), "{events:?}");
        }
        // And nothing else follows them.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(r.count("tick"), 1);
    }

    /// The handshake polls on the fast cadence until the surface is ready,
    /// then stops, and the deferred photo is taken one at a time.
    #[tokio::test(start_paused = true)]
    async fn photos_are_read_one_per_handled_event() {
        let r = rig(true);
        tokio::task::yield_now().await;
        for name in ["1.jpg", "2.jpg", "3.jpg"] {
            r.loader_tx.send(photo(name)).await.unwrap();
        }
        r.set_plan(WakePlan {
            want_photo: true,
            ..idle_plan()
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(r.count("photo"), 3);
        assert!(
            !r.photo_over_read.load(Ordering::SeqCst),
            "a photo was read before the previous event was handled"
        );
    }

    /// A winit thread that never publishes cannot freeze or spin the driver:
    /// it pokes it once a second.
    #[tokio::test(start_paused = true)]
    async fn unhandled_events_are_rechecked_once_a_second() {
        let r = rig(false);
        tokio::time::sleep(Duration::from_millis(5500)).await;
        assert_eq!(r.count("tick"), 6, "{:?}", r.events.lock().unwrap());
    }

    /// A published deadline that has already passed is served by the tick it
    /// caused; it must not turn the wait into a spin.
    #[tokio::test(start_paused = true)]
    async fn a_served_deadline_cannot_spin_the_driver() {
        let r = rig(true);
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        r.set_plan(WakePlan {
            deadline: Some(r.start.into_std()),
            ..idle_plan()
        });
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(r.count("tick"), 1, "{:?}", r.events.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_matting_channel_stops_the_viewer() {
        let mut r = rig(true);
        tokio::task::yield_now().await;
        drop(r.mat_tx.take());
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(r.count("cancelled"), 1);
    }

    #[test]
    fn bounded_polls_and_retries_wake_the_loop_sooner() {
        let now = Instant::now();
        let second = Duration::from_secs(1);
        // Wayland, awake, a photo resting: the size poll beats the dwell.
        let polled = WakeInputs {
            surface_poll: Some(second),
            ..resting(now)
        };
        assert_eq!(plan_wake(polled, now).deadline, Some(now + second));
        // A dwell closer than the poll still wins.
        let near = WakeInputs {
            displayed_at: Some(now - DWELL + Duration::from_millis(300)),
            ..polled
        };
        assert_eq!(
            plan_wake(near, now).deadline,
            Some(now + Duration::from_millis(300))
        );
        // KMS passes no poll; asleep and not-yet-ready surfaces do not poll.
        assert_eq!(plan_wake(resting(now), now).deadline, Some(now + DWELL));
        let asleep = WakeInputs {
            slideshow_active: false,
            sleeping: true,
            displayed_at: None,
            ..polled
        };
        assert_eq!(plan_wake(asleep, now).deadline, None);
        let settling = WakeInputs {
            surface_ready: false,
            ..polled
        };
        assert_eq!(plan_wake(settling, now).deadline, Some(now + DWELL));

        // An overlay whose frame was not presented is asked for again.
        for sleeping in [false, true] {
            let overlay = WakeInputs {
                slideshow_active: false,
                sleeping,
                displayed_at: None,
                overlay_pending: true,
                ..resting(now)
            };
            assert_eq!(
                plan_wake(overlay, now).deadline,
                Some(now + OVERLAY_RETRY),
                "sleeping={sleeping}"
            );
            let presented = WakeInputs {
                overlay_pending: false,
                ..overlay
            };
            assert_eq!(plan_wake(presented, now).deadline, None);
        }
        // The slideshow's own redraws are not this retry's business.
        let wake = WakeInputs {
            overlay_pending: true,
            ..resting(now)
        };
        assert_eq!(plan_wake(wake, now).deadline, Some(now + DWELL));

        // A deferred photo the last step could not submit is retried; one a
        // transition or a full queue is holding back is not.
        let stuck = WakeInputs {
            deferred: 1,
            deferred_head: 1,
            pending: 1,
            ..resting(now)
        };
        assert_eq!(plan_wake(stuck, now).deadline, Some(now + REFILL_RETRY));
        for held in [
            WakeInputs {
                transition_active: true,
                ..stuck
            },
            WakeInputs {
                pending: 3,
                ..stuck
            },
            // A pair at the head does not fit beside two waiting canvases
            // with a preload of 3, where a single photo would (review M4);
            // the next matting result or upload wakes the loop instead.
            WakeInputs {
                pending: 2,
                deferred: 2,
                deferred_head: 2,
                ..stuck
            },
            WakeInputs {
                slideshow_active: false,
                sleeping: true,
                ..stuck
            },
        ] {
            assert_ne!(plan_wake(held, now).deadline, Some(now + REFILL_RETRY));
        }
    }

    struct Host {
        steps: usize,
        /// `tick_now` for the next plans, taken one per call.
        owed: Vec<bool>,
    }

    impl BatchHost for Host {
        fn step(&mut self) {
            self.steps += 1;
        }
        fn plan(&mut self, _now: Instant) -> WakePlan {
            WakePlan {
                tick_now: self.owed.pop().unwrap_or(false),
                ..idle_plan()
            }
        }
    }

    #[test]
    fn a_batch_that_finalized_a_transition_runs_one_more_step() {
        let mut host = Host {
            steps: 0,
            owed: vec![false, true],
        };
        let published = end_of_batch(&mut host, false, 7, Instant::now);
        assert_eq!(host.steps, 1, "the refill the finalize owes");
        assert!(!published.plan.tick_now, "published plans are never owed");
        assert_eq!(published.handled, 7);
    }

    #[test]
    fn a_batch_without_work_runs_no_step_and_a_window_event_runs_one() {
        let mut host = Host {
            steps: 0,
            owed: vec![],
        };
        end_of_batch(&mut host, false, 0, Instant::now);
        assert_eq!(host.steps, 0);
        end_of_batch(&mut host, true, 0, Instant::now);
        assert_eq!(host.steps, 1);
    }

    #[test]
    fn owed_steps_are_bounded() {
        let mut host = Host {
            steps: 0,
            owed: vec![true; 50],
        };
        end_of_batch(&mut host, false, 0, Instant::now);
        assert_eq!(host.steps, MAX_OWED_STEPS);
    }

    /// A viewer state machine in miniature, planned by the real `plan_wake`
    /// and stepped through `end_of_batch`: dwell, transition, finalize,
    /// refill. The refill is gated off while the transition animates, as the
    /// real uploads and matting are, so it needs the step a finalize owes.
    struct Sim {
        displayed_at: Instant,
        transition_started: Option<Instant>,
        just_finished: bool,
        pending: usize,
        starts: Vec<Instant>,
    }

    const TRANSITION: Duration = Duration::from_millis(600);
    const PRELOAD: usize = 3;

    fn clock() -> Instant {
        tokio::time::Instant::now().into_std()
    }

    impl BatchHost for Sim {
        fn step(&mut self) {
            let now = clock();
            // Refill first, like the real step, and not during a transition.
            if self.transition_started.is_none() {
                self.pending = PRELOAD;
            }
            match self.transition_started {
                Some(at) if now >= at + TRANSITION => {
                    self.transition_started = None;
                    self.displayed_at = now;
                    self.just_finished = true;
                }
                None if now >= self.displayed_at + DWELL && self.pending > 0 => {
                    self.transition_started = Some(now);
                    self.pending -= 1;
                    self.starts.push(now);
                }
                _ => {}
            }
        }

        fn plan(&mut self, now: Instant) -> WakePlan {
            plan_wake(
                WakeInputs {
                    slideshow_active: true,
                    sleeping: false,
                    redraw_requested: false,
                    transition_active: self.transition_started.is_some(),
                    transition_just_finished: std::mem::take(&mut self.just_finished),
                    displayed_at: Some(self.displayed_at),
                    dwell: DWELL,
                    surface_ready: true,
                    pending: self.pending,
                    in_flight: 0,
                    deferred: 0,
                    deferred_head: 0,
                    largest_screen: 1,
                    preload: PRELOAD,
                    surface_poll: None,
                    overlay_pending: false,
                    photo_staged: false,
                    lead_in: Duration::ZERO,
                    overlay_expires_at: None,
                },
                now,
            )
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_resting_slideshow_keeps_its_deadlines_and_refills_after_each_transition() {
        let sim = Arc::new(Mutex::new(Sim {
            displayed_at: clock(),
            transition_started: None,
            just_finished: false,
            pending: PRELOAD,
            starts: Vec::new(),
        }));
        let app = Arc::clone(&sim);
        let r = rig_with(Arc::new(Mutex::new(idle_plan())), move |_kind, handled| {
            let mut sim = app.lock().unwrap();
            // user_event runs the step; about_to_wait ends the batch.
            sim.step();
            Some(end_of_batch(&mut *sim, false, handled, clock))
        });
        let mut ticks_before = 0;
        for cycle in 1..=3 {
            tokio::time::sleep(DWELL + TRANSITION + Duration::from_millis(50)).await;
            let sim = sim.lock().unwrap();
            assert_eq!(sim.starts.len(), cycle, "transition {cycle} started");
            assert!(sim.transition_started.is_none(), "cycle {cycle} finalized");
            assert_eq!(
                sim.pending, PRELOAD,
                "cycle {cycle}: refilled by the owed step"
            );
            drop(sim);
            let ticks = r.count("tick");
            // Per cycle: the dwell tick, ~150 fast ticks while animating, and
            // nothing while resting.
            assert!(
                ticks - ticks_before <= 160,
                "{} ticks",
                ticks - ticks_before
            );
            ticks_before = ticks;
        }
        let sim = sim.lock().unwrap();
        for pair in sim.starts.windows(2) {
            let gap = pair[1] - pair[0];
            let ideal = DWELL + TRANSITION;
            assert!(
                gap >= ideal && gap <= ideal + Duration::from_millis(20),
                "transition gap {gap:?}, ideal {ideal:?}"
            );
        }
    }
}
