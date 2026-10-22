//! The pure parts of DRM/KMS frame pacing (`kms.rs`): the transition clock
//! (frame k is meant for vblank anchor + k), where a vblank timestamp sits on
//! the `Instant` clock, when to stop waiting on vblanks, and which vblank
//! each transition frame actually flipped on (the late-frame audit).
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

/// When blocking vblank waits (used only to close a transition's audit,
/// after its last frame) stop being worth trying.
///
/// A failed wait (EINVAL on a disabled CRTC, or EBUSY after the kernel's
/// 3 s timeout when vblanks stop on an enabled one) costs up to 3 s of a
/// stalled loop. One failure stops the waits for the rest of that
/// transition; two in a row, across transitions, stop them for the process,
/// since the cause is then not transient. A success resets the count.
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

/// The transition clock on DRM/KMS: frame `k` of a transition is meant for
/// vblank `anchor + k`, the vblank FIFO shows it on when no frame is late.
///
/// The anchor is the vblank after the one most recently reported when the
/// transition's first frame is rendered. Its time is that vblank's kernel
/// timestamp plus one period, and every later frame is exactly one period
/// later (`period` comes from the mode timings, not a measurement).
#[derive(Clone, Copy, Debug)]
pub(super) struct FrameClock {
    /// `None` when no vblank could be queried; the audit is then off.
    anchor_vblank: Option<u64>,
    anchor_time: Instant,
    period: Duration,
}

impl FrameClock {
    /// The first frame is meant for the vblank after `last` (sequence and
    /// time of the most recent vblank).
    pub(super) fn after_vblank(last: (u64, Instant), period: Duration) -> Self {
        Self {
            anchor_vblank: Some(last.0 + 1),
            anchor_time: last.1 + period,
            period,
        }
    }

    /// No vblank information: the first frame is meant for one period from
    /// `now`, without sequence numbers.
    pub(super) fn from_now(now: Instant, period: Duration) -> Self {
        Self {
            anchor_vblank: None,
            anchor_time: now + period,
            period,
        }
    }

    pub(super) fn anchor_time(&self) -> Instant {
        self.anchor_time
    }

    /// The vblank frame `index` is meant for.
    pub(super) fn target(&self, index: u32) -> Option<u64> {
        self.anchor_vblank.map(|a| a + u64::from(index))
    }

    /// The time of the vblank frame `index` is meant for.
    pub(super) fn index_time(&self, index: u32) -> Instant {
        self.anchor_time + self.period * index
    }
}

/// The clock a frame is drawn for: the time of the vblank it is meant for,
/// unless it is only rendered after that time. Then the display is already
/// behind (no frame can be shown before it is drawn), and the frame is drawn
/// for the moment it is rendered. So a GPU that keeps up gives exact,
/// evenly spaced frames; one that cannot keep up gives frames that track
/// wall time, and the transition keeps its duration instead of stretching.
pub(super) fn frame_time(index_time: Instant, rendered_at: Instant) -> Instant {
    index_time.max(rendered_at)
}

/// Replays FIFO from what the loop can observe, to find the vblank each
/// transition frame actually flipped on.
///
/// Model: frame j is meant for vblank `target_j`. It can flip at the first
/// vblank by which its GPU work has completed (`ready_j`), but never before
/// its predecessor has flipped, so `flip_j = max(target_j, ready_j,
/// flip_{j-1} + 1)`. Each refresh between two consecutive flips repeated a
/// frame: `repeats = Σ_{j≥1} (flip_j - flip_{j-1} - 1)`. The first frame of
/// a transition shows progress 0 (the resting photo), so its own delay only
/// shifts the whole transition and is not counted.
///
/// `ready_j` is known at vblank granularity. While a transition is audited,
/// an observer thread (`kms.rs`) wakes at every vblank `s`, polls the
/// device, and reports `s`; a frame whose completion callback has not fired
/// is seen unfinished at s. `ready_j` is the earliest vblank consistent with
/// that and with when it was submitted: the vblank after the last
/// observation that saw it unfinished, and no earlier than the vblank after
/// the one it was submitted after (it cannot complete before it is
/// submitted). With an observation at every vblank this is exact, except for
/// a frame completing in the fraction of a millisecond between a vblank and
/// the observer's wake (counted as ready one vblank early).
#[derive(Debug, Default)]
pub(super) struct FlipAudit {
    frames: VecDeque<AuditedFrame>,
    last_flip: Option<u64>,
    first_flip: Option<u64>,
    flips: u32,
    repeats: u32,
    best_interval: Option<u64>,
    worst_interval: Option<u64>,
}

#[derive(Debug)]
struct AuditedFrame {
    target: u64,
    /// The most recent vblank when it was submitted.
    submitted_after: u64,
    done: Arc<AtomicBool>,
    submitted_at: Instant,
    /// The last observation (vblank, time) that saw the GPU work unfinished.
    unfinished_at: Option<(u64, Instant)>,
    /// When the completion was first seen.
    seen_done_at: Option<Instant>,
    ready: Option<u64>,
}

/// One frame whose flip is now known, for the per-frame debug log.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ResolvedFrame {
    pub(super) target: u64,
    pub(super) flip: u64,
    /// GPU work took at least this long after submission (it was still
    /// running at the last observation that saw it unfinished).
    pub(super) gpu_at_least: Duration,
    /// ... and at most this long (it was seen complete then); `None` when
    /// the transition ended before it was seen complete.
    pub(super) gpu_at_most: Option<Duration>,
}

/// What a transition looked like on the panel, in refreshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AuditSummary {
    /// Frames that flipped.
    pub(super) frames: u32,
    /// Refreshes on which no new frame appeared (after the first flip).
    pub(super) repeats: u32,
    /// Refreshes from the first flip to the last.
    pub(super) span: u64,
    /// Shortest and longest time a frame stayed on screen, in refreshes
    /// (`None` with fewer than two frames).
    pub(super) best: Option<u64>,
    pub(super) worst: Option<u64>,
}

impl FlipAudit {
    /// A transition frame meant for vblank `target` was submitted at
    /// `submitted_at`, when `submitted_after` was the most recent vblank;
    /// `done` is set by the queue's submitted-work-done callback.
    pub(super) fn submitted(
        &mut self,
        target: u64,
        submitted_after: u64,
        done: Arc<AtomicBool>,
        submitted_at: Instant,
    ) {
        self.frames.push_back(AuditedFrame {
            target,
            submitted_after,
            done,
            submitted_at,
            unfinished_at: None,
            seen_done_at: None,
            ready: None,
        });
    }

    pub(super) fn has_unresolved(&self) -> bool {
        !self.frames.is_empty()
    }

    /// After a device poll, with `vblank` the most recent vblank at `now`:
    /// mark completed frames ready and resolve every frame whose flip is now
    /// known.
    pub(super) fn observe(&mut self, vblank: u64, now: Instant) -> Vec<ResolvedFrame> {
        for frame in self.frames.iter_mut().filter(|f| f.ready.is_none()) {
            if frame.done.load(Ordering::Acquire) {
                frame.seen_done_at = Some(now);
                let after_unfinished = frame.unfinished_at.map_or(0, |(seen, _)| seen + 1);
                frame.ready = Some(after_unfinished.max(frame.submitted_after + 1));
            } else {
                frame.unfinished_at = Some((vblank, now));
            }
        }
        let mut resolved = Vec::new();
        while self.frames.front().is_some_and(|f| f.ready.is_some()) {
            let frame = self.frames.pop_front().expect("front exists");
            let ready = frame.ready.expect("checked");
            resolved.push(self.flip(&frame, ready));
        }
        resolved
    }

    /// The transition is over; `vblank` is the most recent vblank at `now`.
    /// Frames still not complete are counted as flipping no earlier than the
    /// next vblank, a lower bound on their delay. Returns the summary and
    /// the frames resolved by this call, and resets for the next transition.
    pub(super) fn finish(
        &mut self,
        vblank: u64,
        now: Instant,
    ) -> (AuditSummary, Vec<ResolvedFrame>) {
        let mut resolved = self.observe(vblank, now);
        while let Some(frame) = self.frames.pop_front() {
            let ready = frame
                .ready
                .unwrap_or((vblank + 1).max(frame.submitted_after + 1));
            resolved.push(self.flip(&frame, ready));
        }
        let summary = AuditSummary {
            frames: self.flips,
            repeats: self.repeats,
            span: match (self.first_flip, self.last_flip) {
                (Some(first), Some(last)) => last - first,
                _ => 0,
            },
            best: self.best_interval,
            worst: self.worst_interval,
        };
        *self = Self::default();
        (summary, resolved)
    }

    /// Forget everything (the transition could not be audited).
    pub(super) fn discard(&mut self) {
        *self = Self::default();
    }

    fn flip(&mut self, frame: &AuditedFrame, ready: u64) -> ResolvedFrame {
        let earliest = self.last_flip.map_or(0, |last| last + 1);
        let flip = frame.target.max(ready).max(earliest);
        if let Some(last) = self.last_flip {
            let interval = flip - last;
            self.repeats += u32::try_from(interval - 1).unwrap_or(u32::MAX);
            self.best_interval = Some(self.best_interval.map_or(interval, |b| b.min(interval)));
            self.worst_interval = Some(self.worst_interval.map_or(interval, |w| w.max(interval)));
        } else {
            self.first_flip = Some(flip);
        }
        self.last_flip = Some(flip);
        self.flips += 1;
        ResolvedFrame {
            target: frame.target,
            flip,
            gpu_at_least: frame.unfinished_at.map_or(Duration::ZERO, |(_, t)| {
                t.saturating_duration_since(frame.submitted_at)
            }),
            gpu_at_most: frame
                .seen_done_at
                .map(|t| t.saturating_duration_since(frame.submitted_at)),
        }
    }
}

/// Extend the kernel's 32-bit vblank counter to 64 bits so differences stay
/// correct across a wrap (about 2.3 years at 60 Hz). Two threads read the
/// counter (the loop and the flip observer), so a reading may arrive
/// slightly out of order: only a step back of more than half the range is a
/// wrap.
#[derive(Debug, Default)]
pub(super) struct VblankCounter {
    last: Option<u32>,
    high: u64,
}

impl VblankCounter {
    pub(super) fn extend(&mut self, sequence: u32) -> u64 {
        match self.last {
            Some(last) if sequence < last && last - sequence > u32::MAX / 2 => {
                self.high += 1 << 32;
                self.last = Some(sequence);
                self.high + u64::from(sequence)
            }
            Some(last) if sequence < last => {
                // An older reading: before the newest one, same epoch.
                self.high + u64::from(sequence)
            }
            Some(last) if sequence > last && sequence - last > u32::MAX / 2 => {
                // An older reading from before a wrap already seen.
                self.high - (1 << 32) + u64::from(sequence)
            }
            _ => {
                self.last = Some(sequence);
                self.high + u64::from(sequence)
            }
        }
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
    fn frame_clock_spaces_frames_one_period_apart_from_the_next_vblank() {
        let period = Duration::from_nanos(16_666_666);
        let last = Instant::now();
        let clock = FrameClock::after_vblank((500, last), period);
        assert_eq!(clock.target(0), Some(501));
        assert_eq!(clock.target(7), Some(508));
        assert_eq!(clock.index_time(0), last + period);
        assert_eq!(clock.index_time(3), last + period * 4);
        assert_eq!(FrameClock::from_now(last, period).target(0), None);
    }

    #[test]
    fn a_frame_is_never_drawn_for_a_time_before_it_is_rendered() {
        let t = Instant::now();
        let later = t + Duration::from_millis(5);
        assert_eq!(frame_time(later, t), later);
        assert_eq!(frame_time(t, later), later);
    }

    #[test]
    fn audit_counts_repeats_and_display_intervals() {
        let t0 = Instant::now();
        let mut audit = FlipAudit::default();
        let f: Vec<_> = (0..4).map(|_| done(false)).collect();
        // Frames meant for 11..14; frame 2 (target 13) is still running
        // when vblank 13 is observed, done by 14.
        for (k, flag) in f.iter().enumerate() {
            audit.submitted(11 + k as u64, 10, Arc::clone(flag), t0);
        }
        audit.observe(10, t0);
        f[0].store(true, Ordering::Release);
        audit.observe(11, t0);
        f[1].store(true, Ordering::Release);
        audit.observe(12, t0);
        audit.observe(13, t0);
        f[2].store(true, Ordering::Release);
        audit.observe(14, t0);
        f[3].store(true, Ordering::Release);
        let (summary, _) = audit.finish(15, t0);
        // Flips 11, 12, 14, 15: one repeat (frame 1 stayed two refreshes).
        assert_eq!(
            summary,
            AuditSummary {
                frames: 4,
                repeats: 1,
                span: 4,
                best: Some(1),
                worst: Some(2),
            }
        );
        assert!(!audit.has_unresolved());
    }

    #[test]
    fn unfinished_frames_at_the_end_count_as_late() {
        let t0 = Instant::now();
        let mut audit = FlipAudit::default();
        audit.submitted(31, 30, done(true), t0);
        audit.submitted(32, 30, done(false), t0);
        let (summary, resolved) = audit.finish(32, t0);
        // The second frame can flip at 33 at the earliest.
        assert_eq!(summary.repeats, 1);
        assert_eq!(resolved[1].flip, 33);
        assert_eq!(resolved[1].gpu_at_most, None);
        assert_eq!(
            audit.finish(40, t0).0.frames,
            0,
            "reset for the next transition"
        );
    }

    /// What the panel showed: for each vblank from the one that first showed
    /// frame 0 to the one that first showed the last frame, the frame index
    /// and the clock that frame was drawn for.
    struct Panel {
        shown: Vec<(usize, Instant)>,
        frames: usize,
    }

    impl Panel {
        /// Refreshes on which the panel showed no new frame.
        fn repeats(&self) -> u32 {
            (self.shown.len() - self.frames) as u32
        }
    }

    /// A model of the KMS pipeline: a FIFO swapchain of `images` on a 60 Hz
    /// display, the loop rendering ahead (blocked only by the acquire), each
    /// frame costing `cpu` on the CPU and `gpu[k]` on a serial GPU, and Mesa
    /// flipping the oldest queued frame at the first vblank by which it is
    /// complete (the image on screen is then free again). The flip observer
    /// sees every vblank (polling the GPU right at it) and the audit is
    /// closed after the last frame, as `kms.rs` does.
    fn simulate(
        images: usize,
        cpu: Duration,
        gpu: &[Duration],
        duration: Duration,
    ) -> (Panel, AuditSummary) {
        simulate_law(Law::default(), images, cpu, gpu, duration)
    }

    #[derive(Clone, Copy, Default)]
    struct Law {
        /// The v1.2.0-rc1 law: block on the next vblank before rendering
        /// each transition frame.
        wait_for_vblank: bool,
        /// The present returns only once the GPU has finished the frame (a
        /// CPU-side fence wait in the present path), so the loop and the GPU
        /// run in series.
        present_waits_for_gpu: bool,
    }

    fn simulate_law(
        law: Law,
        images: usize,
        cpu: Duration,
        gpu: &[Duration],
        duration: Duration,
    ) -> (Panel, AuditSummary) {
        let period = Duration::from_nanos(16_666_667);
        let base = Instant::now();
        let vblank_time = |n: u64| base + period * n as u32;

        let mut queue: VecDeque<(usize, Instant)> = VecDeque::new(); // (frame, completion)
        let mut free = images - 1; // one image shows the resting photo
        let mut shown: Option<usize> = None;
        let mut history: Vec<Option<usize>> = Vec::new(); // what vblank n showed
        let mut next_vblank: u64 = 1;
        let flip_at = |n: u64,
                       queue: &mut VecDeque<(usize, Instant)>,
                       free: &mut usize,
                       shown: &mut Option<usize>,
                       history: &mut Vec<Option<usize>>| {
            if let Some(&(frame, completion)) = queue.front()
                && completion <= vblank_time(n)
            {
                queue.pop_front();
                *shown = Some(frame);
                *free += 1;
            }
            history.push(*shown);
        };

        // The transition's first frame is rendered 5.3 ms after vblank 0.
        let mut t = vblank_time(0) + Duration::from_micros(5_300);
        let clock = FrameClock::after_vblank((0, vblank_time(0)), period);
        let mut audit = FlipAudit::default();
        let mut gpu_free_at = t;
        let mut drawn_for: Vec<Instant> = Vec::new();
        let mut submitted: Vec<(Arc<AtomicBool>, Instant)> = Vec::new();
        let poll = |submitted: &Vec<(Arc<AtomicBool>, Instant)>, now: Instant| {
            for (flag, completion) in submitted {
                if *completion <= now {
                    flag.store(true, Ordering::Release);
                }
            }
        };
        let mut k = 0usize;
        loop {
            // The old law first waits for the next vblank after now.
            let mut wait_until = if law.wait_for_vblank {
                let mut n = next_vblank;
                while vblank_time(n) <= t {
                    n += 1;
                }
                Some(n)
            } else {
                None
            };
            // Vblanks that have happened by now (or until the wait is over),
            // then the acquire. The observer polls and reports at each one.
            while vblank_time(next_vblank) <= t
                || free == 0
                || wait_until.is_some_and(|n| next_vblank <= n)
            {
                if wait_until == Some(next_vblank) {
                    wait_until = None;
                }
                let n = next_vblank;
                flip_at(n, &mut queue, &mut free, &mut shown, &mut history);
                poll(&submitted, vblank_time(n));
                audit.observe(n, vblank_time(n));
                next_vblank += 1;
                t = t.max(vblank_time(n));
            }
            let ft = frame_time(clock.index_time(k as u32), t);
            drawn_for.push(ft);
            free -= 1;
            t += cpu;
            let completion = gpu_free_at.max(t) + gpu[k.min(gpu.len() - 1)];
            gpu_free_at = completion;
            let flag = done(false);
            audit.submitted(
                clock.target(k as u32).unwrap(),
                next_vblank - 1,
                Arc::clone(&flag),
                t,
            );
            submitted.push((flag, completion));
            queue.push_back((k, completion));
            if law.present_waits_for_gpu {
                t = t.max(completion);
            }
            if ft.saturating_duration_since(clock.anchor_time()) >= duration {
                break;
            }
            k += 1;
        }
        let frames = k + 1;
        // The display drains; the audit is closed vblank by vblank.
        while !queue.is_empty() {
            let n = next_vblank;
            t = t.max(vblank_time(n));
            flip_at(n, &mut queue, &mut free, &mut shown, &mut history);
            poll(&submitted, vblank_time(n));
            audit.observe(n, vblank_time(n));
            next_vblank += 1;
        }
        let (summary, _) = audit.finish(next_vblank - 1, t);

        let first = history.iter().position(|f| *f == Some(0)).unwrap();
        let last = history.iter().position(|f| *f == Some(frames - 1)).unwrap();
        let shown = history[first..=last]
            .iter()
            .map(|f| {
                let f = f.unwrap();
                (f, drawn_for[f])
            })
            .collect();
        (Panel { shown, frames }, summary)
    }

    #[test]
    fn render_ahead_shows_every_frame_on_consecutive_vblanks_with_even_steps() {
        let period = Duration::from_nanos(16_666_667);
        // GPU close to a full refresh per frame (the case the vblank-wait
        // law could not hold): render-ahead keeps 60.
        let gpu = [Duration::from_millis(15)];
        let (panel, summary) = simulate(3, Duration::from_millis(2), &gpu, Duration::from_secs(1));
        assert_eq!(summary.repeats, 0, "{summary:?}");
        assert_eq!(panel.repeats(), 0);
        assert_eq!((summary.best, summary.worst), (Some(1), Some(1)));
        // Each refresh shows the next frame, drawn for exactly one period
        // after the previous one, including the first frames (rendered ahead
        // back to back while idle images were free).
        for w in panel.shown.windows(2) {
            assert_eq!(w[1].0, w[0].0 + 1);
            assert_eq!(w[1].1 - w[0].1, period);
        }
    }

    #[test]
    fn with_the_loop_in_series_with_the_gpu_render_ahead_loses_only_the_excess() {
        // Gate 1 on the Pi (v1.2.0-rc1) saw frames alternate 16.7/33.3 ms.
        // With the present waiting for the GPU, the vblank-wait law starts
        // every frame on a vblank, so a frame of 15 + 2 ms (just over one
        // refresh) costs two refreshes. Render-ahead starts the next frame
        // as soon as the previous one is done and loses only the excess.
        let gpu = [Duration::from_millis(15)];
        let cpu = Duration::from_millis(2);
        let serial = Law {
            present_waits_for_gpu: true,
            ..Law::default()
        };
        let (old, old_summary) = simulate_law(
            Law {
                wait_for_vblank: true,
                ..serial
            },
            3,
            cpu,
            &gpu,
            Duration::from_secs(1),
        );
        let (new, new_summary) = simulate_law(serial, 3, cpu, &gpu, Duration::from_secs(1));
        assert_eq!(old_summary.repeats, old.repeats());
        assert_eq!(new_summary.repeats, new.repeats());
        assert!(old.repeats() >= 25, "old law: {} repeats", old.repeats());
        assert!(
            new.repeats() <= 5,
            "render-ahead: {} repeats",
            new.repeats()
        );
        // And a frame that fits in one refresh in series holds 60 exactly.
        let gpu = [Duration::from_millis(13)];
        let (new, summary) = simulate_law(serial, 3, cpu, &gpu, Duration::from_secs(1));
        assert_eq!((new.repeats(), summary.repeats), (0, 0));
    }

    #[test]
    fn a_slow_frame_is_absorbed_by_the_queue_or_counted() {
        // One 25 ms frame among 12 ms ones: the queue of frames rendered
        // ahead absorbs it, and the audit agrees with the panel.
        let mut gpu = vec![Duration::from_millis(12); 80];
        gpu[20] = Duration::from_millis(25);
        let (panel, summary) = simulate(3, Duration::from_millis(2), &gpu, Duration::from_secs(1));
        assert_eq!(summary.repeats, panel.repeats(), "{summary:?}");
        // A GPU that cannot keep up at all (24 ms every frame): the audit
        // sees the repeats the panel shows, and the frames track wall time,
        // so the transition keeps its length instead of stretching.
        let gpu = [Duration::from_millis(24)];
        let (panel, summary) = simulate(3, Duration::from_millis(2), &gpu, Duration::from_secs(1));
        assert!(panel.repeats() > 0);
        assert_eq!(summary.repeats, panel.repeats(), "{summary:?}");
        assert_eq!(summary.worst, Some(2));
        assert!(
            (panel.shown.len() as f64) < 1.15 * 60.0,
            "the transition stretched to {} refreshes",
            panel.shown.len()
        );
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
        // A late reading from before the wrap.
        assert_eq!(counter.extend(u32::MAX), u64::from(u32::MAX));
        assert_eq!(counter.extend(2), (1 << 32) + 2);
    }

    #[test]
    fn vblank_counter_tolerates_out_of_order_readings() {
        let mut counter = VblankCounter::default();
        assert_eq!(counter.extend(101), 101);
        assert_eq!(counter.extend(100), 100);
        assert_eq!(counter.extend(102), 102);
    }
}
