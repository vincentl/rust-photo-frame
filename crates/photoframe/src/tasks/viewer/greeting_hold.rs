//! How long the greeting stays on screen, timed by the viewer from the
//! greeting's first present.
//!
//! The control task sends the startup state (`set-state`, the schedule's or
//! awake) `greeting-screen.duration-seconds` after the process starts. That
//! timer knows nothing about the display: at a cold boot the viewer's first
//! frame can come long after it (the DRM/KMS output and the GPU are set up
//! first), and the state used to be applied as it arrived, before the
//! greeting was ever drawn. The greeting was skipped and the panel stayed
//! black until the first photo.
//!
//! So the viewer holds every awake/asleep request that arrives while the
//! greeting is its mode, and applies the last one it asked for (the requests
//! folded in arrival order) once:
//!
//! * the greeting has been presented and on screen for its duration
//!   (measured from its first present, not from the process start); and
//! * for awake, a photo is ready to show, so the slideshow never starts on an
//!   empty, black stage ("the greeting stays until the first photo is
//!   ready"). Asleep does not wait for a photo.
//!
//! Before its first present (the panel dark, the Wi-Fi overlay covering it,
//! the GPU still starting) the greeting has not been seen, so the hold has
//! not started. Display power and the overlay are never held.

use std::time::{Duration, Instant};

use crate::events::ViewerState;

/// An awake/asleep request held during the greeting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StateRequest {
    Set(ViewerState),
    Toggle,
}

#[derive(Debug)]
pub(super) struct GreetingHold {
    duration: Duration,
    /// The first present of the greeting now in progress.
    shown_at: Option<Instant>,
    /// The state asked for during the greeting; `None` while nothing was.
    requested: Option<ViewerState>,
}

impl GreetingHold {
    pub(super) fn new(duration: Duration) -> Self {
        Self {
            duration,
            shown_at: None,
            requested: None,
        }
    }

    /// A new greeting begins (startup, a resume). `resume` is the state the
    /// viewer was in, kept as the request so the greeting gives way to it.
    pub(super) fn restart(&mut self, resume: Option<ViewerState>) {
        self.shown_at = None;
        self.requested = resume;
    }

    /// The greeting reached the panel; the first call starts the hold.
    pub(super) fn note_presented(&mut self, now: Instant) -> bool {
        if self.shown_at.is_some() {
            return false;
        }
        self.shown_at = Some(now);
        true
    }

    /// Fold a request into the one held. A toggle flips the requested state;
    /// with none requested yet it means asleep, which is what a toggle in
    /// the greeting has always done.
    pub(super) fn request(&mut self, request: StateRequest) {
        self.requested = Some(match request {
            StateRequest::Set(state) => state,
            StateRequest::Toggle => match self.requested {
                Some(ViewerState::Asleep) => ViewerState::Awake,
                Some(ViewerState::Awake) | None => ViewerState::Asleep,
            },
        });
    }

    /// When the greeting's time on screen is over; `None` before its first
    /// present.
    fn ends_at(&self) -> Option<Instant> {
        self.shown_at.map(|at| at + self.duration)
    }

    /// The held state, taken, once the greeting may give way to it.
    pub(super) fn release(&mut self, now: Instant, photo_ready: bool) -> Option<ViewerState> {
        let end = self.deadline(photo_ready)?;
        if now < end {
            return None;
        }
        self.requested.take()
    }

    /// When the held request can be released: the end of the greeting's
    /// time on screen, while a request waits on it and nothing else does.
    /// `None` before the first present, with nothing held, or while an
    /// awake request waits on a photo (the step that uploads one releases
    /// it). Never filtered against the clock: an end already past is due
    /// now, and the wake plan turns it into a tick (`wake::plan_wake`).
    pub(super) fn deadline(&self, photo_ready: bool) -> Option<Instant> {
        let state = self.requested?;
        if state == ViewerState::Awake && !photo_ready {
            return None;
        }
        self.ends_at()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIX: Duration = Duration::from_secs(6);

    #[test]
    fn a_request_before_the_first_present_waits_for_the_greeting() {
        // Cold boot: the startup state arrives while the GPU is still
        // starting, long before the greeting's first frame.
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(SIX);
        hold.request(StateRequest::Set(ViewerState::Awake));
        assert_eq!(hold.release(t0 + Duration::from_secs(30), true), None);
        assert_eq!(hold.deadline(true), None);

        let shown = t0 + Duration::from_secs(31);
        assert!(hold.note_presented(shown));
        assert_eq!(hold.deadline(true), Some(shown + SIX));
        assert_eq!(hold.release(shown + Duration::from_secs(5), true), None);
        assert_eq!(
            hold.release(shown + SIX, true),
            Some(ViewerState::Awake),
            "the greeting gives way after its duration on screen"
        );
        assert_eq!(hold.release(shown + SIX, true), None, "released once");
    }

    #[test]
    fn awake_also_waits_for_the_first_photo() {
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(SIX);
        hold.note_presented(t0);
        hold.request(StateRequest::Set(ViewerState::Awake));
        assert_eq!(hold.release(t0 + SIX * 3, false), None);
        assert_eq!(hold.deadline(false), None, "a photo wakes the loop");
        assert_eq!(hold.deadline(true), Some(t0 + SIX), "past ends are kept");
        assert_eq!(hold.release(t0 + SIX * 3, true), Some(ViewerState::Awake));
    }

    #[test]
    fn asleep_does_not_wait_for_a_photo() {
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(SIX);
        hold.note_presented(t0);
        hold.request(StateRequest::Set(ViewerState::Asleep));
        assert_eq!(hold.release(t0 + SIX, false), Some(ViewerState::Asleep));
    }

    #[test]
    fn requests_fold_in_arrival_order() {
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(SIX);
        hold.note_presented(t0);
        // The startup state, then a button press during the greeting.
        hold.request(StateRequest::Set(ViewerState::Awake));
        hold.request(StateRequest::Toggle);
        assert_eq!(hold.release(t0 + SIX, true), Some(ViewerState::Asleep));

        hold.restart(None);
        hold.note_presented(t0);
        hold.request(StateRequest::Toggle);
        assert_eq!(hold.release(t0 + SIX, true), Some(ViewerState::Asleep));
    }

    #[test]
    fn nothing_requested_holds_nothing() {
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(SIX);
        hold.note_presented(t0);
        assert_eq!(hold.release(t0 + SIX, true), None);
        assert_eq!(hold.deadline(true), None);
    }

    #[test]
    fn a_restart_times_the_new_greeting_and_resumes_the_old_state() {
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(SIX);
        assert!(hold.note_presented(t0));
        assert!(!hold.note_presented(t0 + SIX), "only the first present");
        // A GPU rebuild during the slideshow: greeting again, then awake.
        hold.restart(Some(ViewerState::Awake));
        let again = t0 + Duration::from_secs(60);
        assert_eq!(hold.release(again, true), None);
        hold.note_presented(again);
        assert_eq!(hold.release(again + SIX, true), Some(ViewerState::Awake));
    }

    #[test]
    fn a_zero_duration_waits_only_for_the_present_and_a_photo() {
        let t0 = Instant::now();
        let mut hold = GreetingHold::new(Duration::ZERO);
        hold.request(StateRequest::Set(ViewerState::Awake));
        assert_eq!(hold.release(t0, true), None, "not presented yet");
        hold.note_presented(t0);
        assert_eq!(hold.deadline(false), None);
        assert_eq!(hold.deadline(true), Some(t0), "due at once");
        assert_eq!(hold.release(t0, false), None);
        assert_eq!(hold.release(t0, true), Some(ViewerState::Awake));
    }
}
