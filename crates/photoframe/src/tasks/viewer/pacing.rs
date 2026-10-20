//! The pure parts of DRM/KMS frame pacing (`kms.rs`): where a vblank
//! timestamp sits on the `Instant` clock, when to stop using vblank waits,
//! and how many refreshes the panel showed no new transition frame.
//! Platform-independent so they are tested everywhere; the ioctls that feed
//! them live in `kms.rs` and `crate::drm_display`.

// Driven by the KMS loop, which only builds on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Place a CLOCK_MONOTONIC `timestamp` on the `Instant` clock, given the two
/// clocks read at (nearly) the same moment. Both run at the same rate
/// (`Instant` is CLOCK_MONOTONIC on Linux), so the timestamp keeps its
/// offset from "now" in either direction. A timestamp ahead of
/// `monotonic_now` is normal: vc4 reports a vblank's time as the end of
/// vblank (start of scanout), which is still in the future when a waiter
/// wakes inside the vblank interval.
pub(super) fn place_on_instant(
    instant_now: Instant,
    monotonic_now: Duration,
    timestamp: Duration,
) -> Instant {
    if timestamp >= monotonic_now {
        instant_now + (timestamp - monotonic_now)
    } else {
        let age = monotonic_now - timestamp;
        instant_now.checked_sub(age).unwrap_or(instant_now)
    }
}

/// When vblank waits stop being worth trying.
///
/// A failed wait (EINVAL on a disabled CRTC, or EBUSY after the kernel's
/// 3 s timeout when vblanks stop on an enabled one) costs up to 3 s of a
/// stalled loop. One failure turns pacing off for the rest of that
/// transition (FIFO alone paces it); two in a row, across transitions, turn
/// it off for the process, since the cause is then not transient. A success
/// resets the count.
#[derive(Debug, Default)]
pub(super) struct PacerHealth {
    consecutive_failures: u32,
    off_for_transition: bool,
    off_for_process: bool,
}

/// Consecutive failed vblank waits that disable pacing for the process.
pub(super) const PACER_FAILURES_TO_DISABLE: u32 = 2;

impl PacerHealth {
    pub(super) fn may_wait(&self) -> bool {
        !self.off_for_transition && !self.off_for_process
    }

    pub(super) fn succeeded(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Returns true when this failure disabled pacing for the process.
    pub(super) fn failed(&mut self) -> bool {
        self.consecutive_failures += 1;
        self.off_for_transition = true;
        if self.consecutive_failures >= PACER_FAILURES_TO_DISABLE && !self.off_for_process {
            self.off_for_process = true;
            return true;
        }
        false
    }

    /// A new transition may try again (unless pacing is off for good).
    pub(super) fn transition_ended(&mut self) {
        self.off_for_transition = false;
    }
}

/// Counts the refreshes during a transition on which the panel showed no new
/// frame, from what the loop can observe.
///
/// Model (FIFO): frame j is rendered right after vblank s_j and aims for
/// vblank `target_j = s_j + 1`. It can flip at the first vblank by which its
/// GPU work has completed (`ready_j`), but never before its predecessor has
/// flipped, so `flip_j = max(target_j, ready_j, flip_{j-1} + 1)`. Each
/// refresh between two consecutive flips repeated a frame:
/// `repeats = Σ_{j≥2} (flip_j - flip_{j-1} - 1)`. The first frame of a
/// transition shows progress 0 (the resting photo), so its own delay is
/// invisible and not counted.
///
/// `ready_j` is known at vblank granularity. At each pacing wake (just after
/// vblank s) the loop polls the device; a frame whose completion callback
/// has not fired is seen unfinished at s. `ready_j` is the earliest vblank
/// consistent with that: the one after the last wake that saw frame j
/// unfinished, or its target if no wake did (the loop may itself have been
/// late and skipped the wake at the target). The model is optimistic only
/// within what was not observed, and about the microseconds between a
/// vblank and the wake after it.
///
/// This sees what the vblank-to-vblank interval cannot: a GPU frame that
/// misses its vblank while the CPU loop stays on time. After such a miss
/// one frame stays queued, which gives every later frame of the transition
/// two refreshes of GPU time (the slack FIFO alone had), so a single miss
/// costs exactly one repeated refresh rather than cascading.
#[derive(Debug, Default)]
pub(super) struct FlipAudit {
    frames: VecDeque<AuditedFrame>,
    last_flip: Option<u64>,
    repeats: u32,
}

#[derive(Debug)]
struct AuditedFrame {
    target: u64,
    done: Arc<AtomicBool>,
    /// The last pacing wake that saw this frame's GPU work unfinished.
    unfinished_at: Option<u64>,
    ready: Option<u64>,
}

impl FlipAudit {
    /// A transition frame was submitted right after vblank `after_vblank`;
    /// `done` is set by the queue's submitted-work-done callback.
    pub(super) fn submitted(&mut self, after_vblank: u64, done: Arc<AtomicBool>) {
        self.frames.push_back(AuditedFrame {
            target: after_vblank + 1,
            done,
            unfinished_at: None,
            ready: None,
        });
    }

    pub(super) fn has_unresolved(&self) -> bool {
        !self.frames.is_empty()
    }

    /// At the wake just after vblank `vblank` (device already polled): mark
    /// completed frames ready and resolve every frame whose flip is known.
    pub(super) fn observe(&mut self, vblank: u64) {
        for frame in self.frames.iter_mut().filter(|f| f.ready.is_none()) {
            if frame.done.load(Ordering::Acquire) {
                frame.ready = Some(frame.unfinished_at.map_or(frame.target, |seen| seen + 1));
            } else {
                frame.unfinished_at = Some(vblank);
            }
        }
        while let Some(ready) = self.frames.front().and_then(|f| f.ready) {
            let frame = self.frames.pop_front().expect("front exists");
            self.flip(frame.target, ready);
        }
    }

    /// The transition is over (observed at `vblank`). Frames still not
    /// complete are counted as flipping no earlier than the next vblank, a
    /// lower bound on their delay. Returns the repeated refreshes since the
    /// last call and resets for the next transition.
    pub(super) fn finish(&mut self, vblank: u64) -> u32 {
        self.observe(vblank);
        while let Some(frame) = self.frames.pop_front() {
            let ready = frame.ready.unwrap_or(vblank + 1);
            self.flip(frame.target, ready);
        }
        self.last_flip = None;
        std::mem::take(&mut self.repeats)
    }

    fn flip(&mut self, target: u64, ready: u64) {
        let earliest = self.last_flip.map_or(0, |last| last + 1);
        let flip = target.max(ready).max(earliest);
        if let Some(last) = self.last_flip {
            self.repeats += u32::try_from(flip - last - 1).unwrap_or(u32::MAX);
        }
        self.last_flip = Some(flip);
    }
}

/// Extend the kernel's 32-bit vblank counter to 64 bits so differences stay
/// correct across a wrap (about 2.3 years at 60 Hz).
#[derive(Debug, Default)]
pub(super) struct VblankCounter {
    last: Option<u32>,
    high: u64,
}

impl VblankCounter {
    pub(super) fn extend(&mut self, sequence: u32) -> u64 {
        if let Some(last) = self.last
            && sequence < last
        {
            self.high += 1 << 32;
        }
        self.last = Some(sequence);
        self.high + u64::from(sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(flag: bool) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(flag))
    }

    #[test]
    fn future_timestamps_keep_their_offset() {
        let now = Instant::now();
        let mono = Duration::from_secs(1_000);
        let ahead = mono + Duration::from_micros(670);
        assert_eq!(
            place_on_instant(now, mono, ahead),
            now + Duration::from_micros(670)
        );
        let behind = mono - Duration::from_millis(3);
        assert_eq!(
            place_on_instant(now, mono, behind),
            now - Duration::from_millis(3)
        );
        assert_eq!(place_on_instant(now, mono, mono), now);
    }

    #[test]
    fn on_time_frames_repeat_nothing() {
        let mut audit = FlipAudit::default();
        let flags: Vec<_> = (0..5).map(|_| done(false)).collect();
        for (k, flag) in flags.iter().enumerate() {
            let s = 100 + k as u64;
            // Wake after vblank s: the previous frame finished in time.
            audit.observe(s);
            audit.submitted(s, Arc::clone(flag));
            flag.store(true, Ordering::Release); // done before vblank s+1
        }
        assert_eq!(audit.finish(105), 0);
    }

    #[test]
    fn one_gpu_miss_is_one_repeat_and_does_not_cascade() {
        let mut audit = FlipAudit::default();
        // Frames rendered after vblanks 10..15; frame 2 (after 11) is done
        // only by vblank 13 instead of 12. Later frames finish within two
        // refreshes, which the one queued frame now allows.
        let f: Vec<_> = (0..6).map(|_| done(false)).collect();
        audit.submitted(10, Arc::clone(&f[0]));
        f[0].store(true, Ordering::Release);
        audit.observe(11);
        audit.submitted(11, Arc::clone(&f[1]));
        audit.observe(12); // f1 not done: missed vblank 12
        audit.submitted(12, Arc::clone(&f[2]));
        f[1].store(true, Ordering::Release);
        audit.observe(13);
        audit.submitted(13, Arc::clone(&f[3]));
        f[2].store(true, Ordering::Release);
        audit.observe(14);
        audit.submitted(14, Arc::clone(&f[4]));
        f[3].store(true, Ordering::Release);
        audit.observe(15);
        audit.submitted(15, Arc::clone(&f[5]));
        f[4].store(true, Ordering::Release);
        f[5].store(true, Ordering::Release);
        assert_eq!(audit.finish(16), 1);
    }

    #[test]
    fn a_frame_the_cpu_was_late_for_repeats_once() {
        let mut audit = FlipAudit::default();
        let a = done(true);
        let b = done(true);
        audit.submitted(20, a);
        // The loop missed vblank 21 and rendered after 22.
        audit.observe(22);
        audit.submitted(22, b);
        assert_eq!(audit.finish(23), 1);
    }

    #[test]
    fn unfinished_frames_at_the_end_count_as_late() {
        let mut audit = FlipAudit::default();
        let a = done(true);
        let b = done(false);
        audit.submitted(30, a);
        audit.observe(31);
        audit.submitted(31, b);
        // Transition over at vblank 32 and frame b still not done: it can
        // flip at 33 at the earliest, one refresh after its target.
        assert_eq!(audit.finish(32), 1);
        assert!(!audit.has_unresolved());
        // Reset for the next transition.
        assert_eq!(audit.finish(40), 0);
    }

    #[test]
    fn pacer_health_disables_after_consecutive_failures() {
        let mut health = PacerHealth::default();
        assert!(health.may_wait());
        assert!(!health.failed());
        assert!(!health.may_wait(), "off for the rest of this transition");
        health.transition_ended();
        assert!(health.may_wait(), "next transition tries again");
        assert!(
            health.failed(),
            "second consecutive failure disables for the process"
        );
        health.transition_ended();
        assert!(!health.may_wait());

        let mut health = PacerHealth::default();
        health.failed();
        health.transition_ended();
        health.succeeded();
        health.failed();
        health.transition_ended();
        assert!(health.may_wait(), "a success in between resets the count");
    }

    #[test]
    fn vblank_counter_survives_a_wrap() {
        let mut counter = VblankCounter::default();
        assert_eq!(counter.extend(u32::MAX - 1), u64::from(u32::MAX - 1));
        assert_eq!(counter.extend(u32::MAX), u64::from(u32::MAX));
        assert_eq!(counter.extend(0), 1 << 32);
        assert_eq!(counter.extend(1), (1 << 32) + 1);
    }
}
