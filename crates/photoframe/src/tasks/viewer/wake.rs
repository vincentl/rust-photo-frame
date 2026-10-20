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
//! [`next_wake_event`]. T5 applies the same plan to the Wayland driver
//! (`drive_viewer_events`), replacing its unconditional 4 ms tick: fast
//! ticks only while `redraw_now`, otherwise the same event sources.

// Driven by the KMS loop, which only builds on Linux; T5 brings the Wayland
// driver onto it on every platform.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use super::MatResult;
use crate::events::{PhotoLoaded, ViewerCommand};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

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
    /// Photos uploaded and waiting their turn.
    pub(super) pending: usize,
    /// Photos submitted to the matting workers.
    pub(super) in_flight: usize,
    /// Photos taken from the loader but not yet submitted.
    pub(super) deferred: usize,
    /// `viewer-preload-count`.
    pub(super) preload: usize,
}

/// What the loop does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WakePlan {
    /// Render again without waiting.
    pub(super) redraw_now: bool,
    /// Run the control step again without waiting. One-shot (its input is
    /// taken), so it cannot keep the loop awake.
    pub(super) tick_now: bool,
    /// Wake at this instant even if no event arrives: the dwell of the photo
    /// on screen ends. Always in the future when set, so a wait on it can
    /// never spin.
    pub(super) deadline: Option<Instant>,
    /// Take the next decoded photo from the loader when one arrives. This is
    /// exactly `MattingBridge::queue_for_wake`'s own gate for reading the
    /// loader channel, so the channel's backpressure is unchanged: while it
    /// is false the photo stays queued in the loader.
    pub(super) want_photo: bool,
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
    let deadline = match inputs.displayed_at {
        Some(shown) if inputs.slideshow_active && !inputs.transition_active => {
            Some(shown + inputs.dwell).filter(|deadline| *deadline > now)
        }
        _ => None,
    };

    // The step that finalized a transition ran its uploads and matting
    // refill *before* the finalize, while both were still gated off, so the
    // deferred work runs on one more step right after it.
    let tick_now = inputs.transition_just_finished && !inputs.sleeping;

    // Exactly when MattingBridge::queue_for_wake reads the loader (the test
    // `want_photo_matches_the_bridge_gate` in viewer.rs holds the two
    // together): never while a transition animates; with a usable surface
    // it first submits every deferred photo and keeps reading while the
    // queue is short; without one it reads a single photo into `deferred`
    // and stops there.
    let queued = inputs.pending + inputs.in_flight;
    let want_photo = !inputs.sleeping
        && !inputs.transition_active
        && if inputs.surface_ready {
            queued + inputs.deferred < inputs.preload
        } else {
            inputs.deferred == 0 && queued < inputs.preload
        };

    WakePlan {
        redraw_now,
        tick_now,
        deadline,
        want_photo,
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
    loader: Option<&mut Receiver<PhotoLoaded>>,
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

#[cfg(test)]
mod tests {
    use super::super::ImagePlane;
    use super::*;
    use crate::events::{PreparedImageCpu, ViewerState};
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
            preload: 3,
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
                deadline: Some(now + DWELL),
                want_photo: false,
            }
        );
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
                deadline: None,
                want_photo: false,
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
            mat_kind: None,
        }
    }

    struct Sources {
        cancel: CancellationToken,
        control_tx: mpsc::Sender<ViewerCommand>,
        control: Receiver<ViewerCommand>,
        mat_tx: mpsc::Sender<MatResult>,
        mat: Receiver<MatResult>,
        loader_tx: mpsc::Sender<PhotoLoaded>,
        loader: Receiver<PhotoLoaded>,
    }

    fn sources() -> Sources {
        let (control_tx, control) = mpsc::channel(4);
        let (mat_tx, mat) = mpsc::channel(4);
        let (loader_tx, loader) = mpsc::channel(4);
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
            deadline: None,
            want_photo: false,
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
}
