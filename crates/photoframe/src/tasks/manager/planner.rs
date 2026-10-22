//! The screen planner: back-to-back portrait pairing as a pure function of
//! the scheduler (`local-markdown/p1-pairing-design.md` §2), with the opt-in
//! look-ahead window of the 2026-10-22 revision ("Look-ahead pairing").
//!
//! The scheduler is not changed at all. The planner only groups appearances
//! of the sequence it already produces: **when the scheduler's next two
//! appearances are both pairable, they share one screen.** With
//! `pairing.lookahead` the partner may instead sit anywhere in the next `W`
//! positions (`W` = `viewer-preload-count`, the screens already queued or in
//! flight); the photos between are shifted one appearance later. Between
//! screens the planner keeps `Q`, the committed but unsent appearances in
//! sequence order (rc2's single `C`; at most `W − 1`), and `H`, one held peek
//! (not committed). Each step below is named as in the design.
//!
//! Invariants (proofs in the design):
//! - **P1-sched, the scheduler is unchanged.** Every commit happens in
//!   sequence order (the scan only ever commits the scheduler's front), so
//!   the scheduler's sequence, clock and bumps equal pairing off for the same
//!   seed. With `W = 1` the sent sequence is *identical* to it (rc2's P1).
//! - **P1-move, bounded moves.** With `W > 1` the sent sequence is a
//!   permutation of the unpaired one in which every appearance sits within
//!   `W − 1` positions of its unpaired index ([`Displacement`] measures it).
//! - **I5, no photo on consecutive screens** except where the unpaired
//!   sequence already repeats (`Z, Z`): rules 4 and 5, and for a pull from
//!   position `k > 1` the checks `X₁ ∉ {A, X_k}` and `X_{k+1} ≠ X_{k−1}`.
//!
//! All hold exactly here and in `simulate_screens` / the dry run. The live
//! loop drives the same planner, but today's re-peeks during a blocked send
//! and inventory events between planning and display make the live sequence
//! match in distribution only (design B7, V1).

use super::PlaylistScheduler;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One appearance: a photo the scheduler produced, with its priority
/// (`true` until first shown) as of its peek.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appearance {
    pub path: Arc<PathBuf>,
    pub priority: bool,
}

/// One screen: a solo photo, or a pair. The first appearance (`primary`) is
/// drawn on the left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedScreen {
    pub primary: Appearance,
    pub partner: Option<Appearance>,
}

impl PlannedScreen {
    pub fn solo(primary: Appearance) -> Self {
        Self {
            primary,
            partner: None,
        }
    }

    /// The screen's appearances in sequence order.
    pub fn members(&self) -> impl Iterator<Item = &Appearance> {
        std::iter::once(&self.primary).chain(self.partner.as_ref())
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.members().any(|member| member.path.as_path() == path)
    }
    /// Remove `path` from the screen (design B4): a pair degrades to its other
    /// member, keeping the surviving order. `None` when the screen is emptied.
    pub fn without(self, path: &Path) -> Option<Self> {
        let PlannedScreen { primary, partner } = self;
        match (primary.path.as_path() == path, partner) {
            (false, Some(partner)) if partner.path.as_path() == path => Some(Self::solo(primary)),
            (false, partner) => Some(Self { primary, partner }),
            (true, Some(partner)) if partner.path.as_path() != path => Some(Self::solo(partner)),
            (true, _) => None,
        }
    }
}

/// What [`ScreenPlanner::plan`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// **Today's path (step 2, B9).** `P` came from a peek and is *not*
    /// committed: send it, then [`ScreenPlanner::commit_after_send`], exactly
    /// as the loop does with pairing off. Nothing is held.
    SendThenCommit(Appearance),
    /// Every member is already committed (**commit before send**). Send the
    /// screen as is.
    Committed(PlannedScreen),
}

impl Plan {
    pub fn screen(&self) -> PlannedScreen {
        match self {
            Plan::SendThenCommit(primary) => PlannedScreen::solo(primary.clone()),
            Plan::Committed(screen) => screen.clone(),
        }
    }
}

/// splitmix64 finalizer (Steele, Lea & Flood), used to derive the coin seed.
pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// **Coin stream (step 8, B5).** The pairing coin has its own RNG, seeded
/// with `splitmix64(seed ^ 0x9E37_79B9_7F4A_7C15)` when a seed is set, else
/// from the OS. Pairing therefore never draws from the scheduler's RNG, and
/// the coin is not a copy of the scheduler's draw sequence.
pub fn coin_rng(seed: Option<u64>) -> StdRng {
    match seed {
        Some(seed) => StdRng::seed_from_u64(splitmix64(seed ^ 0x9E37_79B9_7F4A_7C15)),
        None => StdRng::from_os_rng(),
    }
}

/// Plans one screen at a time from a [`PlaylistScheduler`].
pub struct ScreenPlanner {
    /// `Q`: committed but unsent, in sequence order; always sent, in order,
    /// on the next screens. rc2's single `C`; with look-ahead at most
    /// `W − 1` long.
    committed: VecDeque<Appearance>,
    /// `H`: the held peek (not committed), the scheduler's current front.
    held: Option<Appearance>,
    /// **"Previous screen" (rule 4, V2):** the members of the last screen
    /// actually sent, recorded for every screen including today's-path solos
    /// (after a B4 degrade it is the degraded screen; a dropped empty screen
    /// leaves it unchanged).
    previous: Vec<Arc<PathBuf>>,
    /// `p`, `pairing.probability`.
    probability: f64,
    /// `W`, the look-ahead window: positions after the head a partner may
    /// come from. 1 is back to back only (rc2).
    lookahead: usize,
    coin: StdRng,
}

impl ScreenPlanner {
    /// Back-to-back pairing (`W = 1`).
    pub fn new(probability: f64, seed: Option<u64>) -> Self {
        Self {
            committed: VecDeque::new(),
            held: None,
            previous: Vec::new(),
            probability,
            lookahead: 1,
            coin: coin_rng(seed),
        }
    }

    /// Look-ahead pairing over `window` positions (`viewer-preload-count`
    /// when `pairing.lookahead` is on). `window <= 1` is exactly [`Self::new`].
    pub fn with_lookahead(mut self, window: usize) -> Self {
        self.lookahead = window.max(1);
        self
    }

    /// Plan the next screen. `pairable` is `None` when pairing is off or the
    /// canvas is not yet published; otherwise it answers the pairable rule
    /// (R2) for a path at this moment (pairability is read when each decision
    /// is made). `None` when the scheduler has nothing to show.
    pub fn plan(
        &mut self,
        sched: &mut dyn PlaylistScheduler,
        pairable: Option<&dyn Fn(&Path) -> bool>,
    ) -> Option<Plan> {
        // Step 1. Head: A is the front of Q if any, else H if present, else a
        // fresh peek.
        let (first, first_committed) = if let Some(c) = self.committed.pop_front() {
            (c, true)
        } else if let Some(h) = self.held.take() {
            (h, false)
        } else {
            (peek(sched)?, false)
        };

        // Step 2. Today's path: A alone if pairing is off, p = 0, the canvas is
        // not yet published (`pairable` is None), or A is not pairable. An A
        // from a peek is sent, then committed (B9); a committed A stays so.
        let pairable = match pairable {
            Some(pairable) if self.probability > 0.0 && pairable(&first.path) => pairable,
            _ => {
                return Some(if first_committed {
                    Plan::Committed(PlannedScreen::solo(first))
                } else {
                    Plan::SendThenCommit(first)
                });
            }
        };

        // Step 3. Commit A (if it is not already), so the positions after it
        // can be peeked.
        if !first_committed {
            sched.commit_shown();
        }

        // Step 4. Scan positions k = 1..=W for a partner X_k. Position k is
        // made visible by committing H into Q and peeking again (commits
        // only ever happen in sequence order). With W = 1 this is rc2's
        // step 4 and 5 on X alone.
        for k in 1..=self.lookahead {
            let Some(candidate) = self.visible(sched, k) else {
                break;
            };
            // Pair conditions (B1-a): X_k pairable, X_k ≠ A, X_k not on the
            // previous screen (rule 4), then the pairing coin.
            let structural = pairable(&candidate.path)
                && candidate.path != first.path
                && !self.previous.contains(&candidate.path);
            if !structural {
                continue;
            }
            // A pull from k > 1 makes X₁ the next screen's first member: it
            // must not be on this screen (I5 across the boundary).
            if k > 1 {
                let x1 = &self.committed[0];
                if x1.path == first.path || x1.path == candidate.path {
                    continue;
                }
            }
            if !self.coin_succeeds() {
                continue;
            }
            // Step 5. Look past X_k (rule 5, B1-b): commit X_k (it is sent
            // now) and peek X_{k+1}, which becomes H when it is new. For
            // k = 1 the pair is refused when X₂ = A (X₁ stays committed in Q
            // and leads the next screen, rc2's C). For k > 1 it is refused
            // when X_{k+1} = X_{k−1}: removing X_k must not make the queue
            // read Z, Z. A refused X_k stays in Q, in order.
            self.commit_through(sched, k);
            let after = self.visible(sched, k + 1);
            let blocked = match after {
                Some(after) if k == 1 => after.path == first.path,
                Some(after) => after.path == self.committed[k - 2].path,
                None => false,
            };
            if blocked {
                continue;
            }
            let partner = self
                .committed
                .remove(k - 1)
                .expect("X_k was committed into Q");
            return Some(Plan::Committed(PlannedScreen {
                primary: first,
                partner: Some(partner),
            }));
        }
        Some(Plan::Committed(PlannedScreen::solo(first)))
    }

    /// Make position `k` (1-based, after the head) visible: `Q[k−1]` if it is
    /// committed, else `H` once it is the `k`-th appearance, committing `H`
    /// into `Q` and peeking again as often as needed. `None` when the
    /// scheduler runs out first.
    fn visible(&mut self, sched: &mut dyn PlaylistScheduler, k: usize) -> Option<Appearance> {
        loop {
            if let Some(queued) = self.committed.get(k - 1) {
                return Some(queued.clone());
            }
            match &self.held {
                None => self.held = Some(peek(sched)?),
                Some(held) if self.committed.len() + 1 == k => return Some(held.clone()),
                Some(_) => {
                    let held = self.held.take().expect("checked");
                    sched.commit_shown();
                    self.committed.push_back(held);
                }
            }
        }
    }

    /// Commit every visible position up to `k` into `Q` (at most `H`).
    fn commit_through(&mut self, sched: &mut dyn PlaylistScheduler, k: usize) {
        while self.committed.len() < k {
            let next = match self.held.take() {
                Some(held) => held,
                None => peek(sched).expect("position k was visible"),
            };
            sched.commit_shown();
            self.committed.push_back(next);
        }
    }

    /// Today's path, second half: the scheduler advances past the photo that
    /// was just sent ([`Plan::SendThenCommit`]).
    pub fn commit_after_send(&mut self, sched: &mut dyn PlaylistScheduler) {
        sched.commit_shown();
    }

    /// Record the screen that was actually sent (rule 4's previous screen).
    pub fn record_sent(&mut self, screen: &PlannedScreen) {
        self.previous = screen.members().map(|m| Arc::clone(&m.path)).collect();
    }

    /// **What invalidates H (step 6, B7):** only `PhotoAdded` and
    /// `PhotoRemoved`; the next plan re-peeks, as today's loop re-peeks.
    /// `PhotoMeta` and `CanvasShape` change no scheduling state and do not.
    /// Committed appearances in `Q` are kept.
    pub fn invalidate_held(&mut self) {
        self.held = None;
    }

    /// **Removal (step 7, B4):** a removed photo committed in `Q` is dropped;
    /// the surviving appearances keep their order. (The caller degrades a
    /// planned, unsent screen with [`PlannedScreen::without`].)
    pub fn on_removed(&mut self, path: &Path) {
        self.committed.retain(|c| c.path.as_path() != path);
        self.invalidate_held();
    }

    /// Whether a held peek is outstanding (for tests of B7/B9).
    #[cfg(test)]
    pub fn holds_peek(&self) -> bool {
        self.held.is_some()
    }

    /// The committed, unsent queue `Q` (for tests).
    #[cfg(test)]
    pub fn queued(&self) -> Vec<String> {
        self.committed
            .iter()
            .map(|c| c.path.display().to_string())
            .collect()
    }

    /// The coin, drawn with probability `p`; the draw is skipped at p = 1.
    fn coin_succeeds(&mut self) -> bool {
        self.probability >= 1.0 || self.coin.random::<f64>() < self.probability
    }
}

fn peek(sched: &mut dyn PlaylistScheduler) -> Option<Appearance> {
    sched
        .peek_next()
        .map(|(path, priority)| Appearance { path, priority })
}

/// Measured properties of a planned screen sequence, as the dry run and the
/// analyzer report them (§2 "Dry run", §6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenStats {
    pub screens: usize,
    pub appearances: usize,
    pub pair_screens: usize,
    /// Appearances of pairable photos.
    pub pairable_appearances: usize,
    /// Paired appearances / pairable appearances.
    pub paired_share: f64,
    /// Of pair members that had a partner before, the share whose partner
    /// is the same as last time.
    pub repeat_partner_rate: f64,
    /// Adjacent screens sharing a photo (I5: 0 unless the unpaired sequence
    /// itself has `Z, Z`).
    pub consecutive_repeats: usize,
}

impl ScreenStats {
    pub fn measure(screens: &[PlannedScreen], pairable: &dyn Fn(&Path) -> bool) -> Self {
        let appearances = screens.iter().map(|s| s.members().count()).sum();
        let pair_screens = screens.iter().filter(|s| s.partner.is_some()).count();
        let pairable_appearances = screens
            .iter()
            .flat_map(PlannedScreen::members)
            .filter(|m| pairable(&m.path))
            .count();
        let mut last_partner: std::collections::HashMap<&Path, &Path> = Default::default();
        let (mut repeats, mut seen) = (0usize, 0usize);
        for screen in screens {
            if let Some(partner) = &screen.partner {
                let (a, b) = (screen.primary.path.as_path(), partner.path.as_path());
                for (u, v) in [(a, b), (b, a)] {
                    if let Some(previous) = last_partner.insert(u, v) {
                        seen += 1;
                        repeats += usize::from(previous == v);
                    }
                }
            }
        }
        let consecutive_repeats = screens
            .windows(2)
            .filter(|w| w[1].members().any(|m| w[0].contains(&m.path)))
            .count();
        Self {
            screens: screens.len(),
            appearances,
            pair_screens,
            pairable_appearances,
            paired_share: (2 * pair_screens) as f64 / pairable_appearances.max(1) as f64,
            repeat_partner_rate: repeats as f64 / seen.max(1) as f64,
            consecutive_repeats,
        }
    }

    /// The independent-draws approximation of the paired share,
    /// `s = 2πp/(1 + πp)` (§2 "Rate"); reported beside the measured value.
    pub fn predicted_share(pi: f64, probability: f64) -> f64 {
        Self::predicted_share_lookahead(pi, probability, 1)
    }

    /// The dry run's `# pairing` lines for these stats (design §2 "Dry run",
    /// plus the look-ahead revision's W-aware rate and displacement).
    pub fn report_lines(
        &self,
        pi: f64,
        probability: f64,
        lookahead: usize,
        moved: Displacement,
    ) -> Vec<String> {
        let mut lines = vec![
            format!("#   pi (pairable weight share): {pi:.3}"),
            format!(
                "#   screens: {} for {} appearances ({} pairs)",
                self.screens, self.appearances, self.pair_screens
            ),
        ];
        if lookahead > 1 {
            lines.push(format!(
                "#   paired share of pairable appearances: {:.3} (2q/(1+q) with q = 1-(1-pi*p)^W = {:.3} at W = {lookahead}, an approximation; back to back would give {:.3})",
                self.paired_share,
                Self::predicted_share_lookahead(pi, probability, lookahead),
                Self::predicted_share(pi, probability)
            ));
        } else {
            lines.push(format!(
                "#   paired share of pairable appearances: {:.3} (2*pi*p/(1+pi*p) = {:.3}, an approximation)",
                self.paired_share,
                Self::predicted_share(pi, probability)
            ));
        }
        lines.push(format!(
            "#   repeat-partner rate: {:.3}",
            self.repeat_partner_rate
        ));
        lines.push(format!(
            "#   max shift from the unpaired order: {} appearances (bound W-1 = {}; {} unmatched at the tail)",
            moved.max,
            lookahead.max(1) - 1,
            moved.unmatched
        ));
        lines.push(format!(
            "#   consecutive-screen repeats: {} (must be 0 unless the unpaired sequence repeats a photo back to back; I5)",
            self.consecutive_repeats
        ));
        lines
    }

    /// The same approximation with a look-ahead window of `w` positions
    /// (design "Look-ahead pairing"): a pairable head pairs when any of the
    /// `w` positions after it is pairable and wins the coin, `q = 1 − (1 −
    /// πp)^w`, and the paired share of pairable appearances is `2q/(1 + q)`.
    /// `w = 1` is [`Self::predicted_share`].
    pub fn predicted_share_lookahead(pi: f64, probability: f64, w: usize) -> f64 {
        let q = 1.0 - (1.0 - pi * probability).powi(w.max(1) as i32);
        2.0 * q / (1.0 + q)
    }
}

/// How far the planned order moved appearances from the unpaired sequence
/// (P1-move). Occurrences of each photo are matched in order between the two
/// sequences; `max` is the largest index difference over matched pairs, and
/// `unmatched` counts planned appearances with no counterpart in the unpaired
/// run of the same length (only possible at the tail of a truncated run,
/// where a pull reaches past it). Both are 0 with look-ahead off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Displacement {
    pub max: usize,
    pub unmatched: usize,
}

impl Displacement {
    pub fn measure(planned: &[PathBuf], unpaired: &[PathBuf]) -> Self {
        let mut positions: std::collections::HashMap<&Path, VecDeque<usize>> = Default::default();
        for (index, path) in unpaired.iter().enumerate() {
            positions
                .entry(path.as_path())
                .or_default()
                .push_back(index);
        }
        let mut out = Self::default();
        for (index, path) in planned.iter().enumerate() {
            match positions
                .get_mut(path.as_path())
                .and_then(VecDeque::pop_front)
            {
                Some(original) => out.max = out.max.max(index.abs_diff(original)),
                None => out.unmatched += 1,
            }
        }
        out
    }
}

/// A scheduler that plays a fixed appearance sequence and logs every call,
/// so the planner's peek/commit order is visible to tests.
#[cfg(test)]
pub(crate) mod scripted {
    use super::super::PlaylistScheduler;
    use crate::events::{CreatedSource, PhotoInfo};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    pub(crate) type Log = Arc<Mutex<Vec<String>>>;

    pub(crate) struct Scripted {
        seq: Vec<Arc<PathBuf>>,
        next: usize,
        log: Log,
    }

    impl Scripted {
        /// Appearances named by `names` (whitespace-separated), plus the log.
        pub(crate) fn new(names: &str) -> (Self, Log) {
            let log = Log::default();
            let seq = names
                .split_whitespace()
                .map(|n| Arc::new(PathBuf::from(n)))
                .collect();
            (
                Self {
                    seq,
                    next: 0,
                    log: Arc::clone(&log),
                },
                log,
            )
        }
    }

    impl PlaylistScheduler for Scripted {
        fn record_add(&mut self, info: PhotoInfo) {
            self.log
                .lock()
                .unwrap()
                .push(format!("add {}", info.path.display()));
        }
        fn record_remove(&mut self, path: &Path) {
            self.log
                .lock()
                .unwrap()
                .push(format!("remove {}", path.display()));
        }
        fn peek_next(&mut self) -> Option<(Arc<PathBuf>, bool)> {
            let front = self.seq.get(self.next).cloned();
            self.log.lock().unwrap().push(format!(
                "peek {}",
                front
                    .as_ref()
                    .map_or("-".into(), |p| p.display().to_string())
            ));
            front.map(|p| (p, false))
        }
        fn commit_shown(&mut self) {
            self.log
                .lock()
                .unwrap()
                .push(format!("commit {}", self.seq[self.next].display()));
            self.next += 1;
        }
        fn inventory_len(&self) -> usize {
            0
        }
        fn current_weight(&self, _path: &Path) -> Option<f64> {
            None
        }
        fn age_seconds(&self, _path: &Path) -> Option<f64> {
            None
        }
        fn created_source(&self, _path: &Path) -> Option<CreatedSource> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::Scripted;
    use super::*;

    fn names(screen: &PlannedScreen) -> String {
        screen
            .members()
            .map(|m| m.path.display().to_string())
            .collect::<Vec<_>>()
            .join("+")
    }

    /// Plan every screen of `seq`, sending each at once (as the dry run does).
    fn plan_all(seq: &str, pairable: &dyn Fn(&Path) -> bool, p: f64) -> Vec<String> {
        let (mut sched, _) = Scripted::new(seq);
        let mut planner = ScreenPlanner::new(p, Some(1));
        let mut out = Vec::new();
        while let Some(plan) = planner.plan(&mut sched, Some(pairable)) {
            let screen = plan.screen();
            if let Plan::SendThenCommit(_) = plan {
                planner.commit_after_send(&mut sched);
            }
            planner.record_sent(&screen);
            out.push(names(&screen));
        }
        out
    }

    fn all(_: &Path) -> bool {
        true
    }

    #[test]
    fn back_to_back_pairable_appearances_share_a_screen() {
        let not_l = |p: &Path| !p.to_str().unwrap().starts_with('L');
        assert_eq!(
            plan_all("a b L1 c L2 d e", &not_l, 1.0),
            ["a+b", "L1", "c", "L2", "d+e"]
        );
    }

    #[test]
    fn rule_5_refuses_a_pair_whose_next_appearance_is_its_first_member() {
        // A, B, A: grouping alone gives (A, B), (A). Rule 5 sends A alone,
        // B (committed) next; rule 4 then keeps A off B's screen.
        assert_eq!(plan_all("A B A C D", &all, 1.0), ["A", "B", "A+C", "D"]);
    }

    #[test]
    fn rule_4_refuses_a_partner_from_the_previous_screen() {
        // A, B, C, A: grouping alone gives (A, B), (C, A).
        assert_eq!(plan_all("A B C A E", &all, 1.0), ["A+B", "C", "A+E"]);
    }

    #[test]
    fn steps_commit_and_peek_only_in_sequence_order() {
        let (mut sched, log) = Scripted::new("a b c");
        let mut planner = ScreenPlanner::new(1.0, None);
        let plan = planner.plan(&mut sched, Some(&all)).unwrap();
        assert_eq!(names(&plan.screen()), "a+b");
        assert!(matches!(plan, Plan::Committed(_)));
        // Commit before send: P, then X, each committed before the next peek.
        assert_eq!(
            *log.lock().unwrap(),
            ["peek a", "commit a", "peek b", "commit b", "peek c"]
        );
        assert!(planner.holds_peek(), "Y = c is held");
        planner.record_sent(&plan.screen());
        // c is the held peek: used as is, not redrawn.
        log.lock().unwrap().clear();
        let plan = planner.plan(&mut sched, Some(&all)).unwrap();
        assert_eq!(names(&plan.screen()), "c");
        assert_eq!(*log.lock().unwrap(), ["commit c", "peek -"]);
    }

    /// **S8 (B9).** With pairing enabled and p = 0 the planner takes today's
    /// send-then-commit path: no commit before send, no held peek.
    #[test]
    fn s8_p0_takes_todays_send_then_commit_path() {
        let (mut sched, log) = Scripted::new("a b c d");
        let mut planner = ScreenPlanner::new(0.0, Some(3));
        for expected in ["a", "b", "c", "d"] {
            log.lock().unwrap().clear();
            let plan = planner.plan(&mut sched, Some(&all)).unwrap();
            assert_eq!(
                plan,
                Plan::SendThenCommit(Appearance {
                    path: Arc::new(PathBuf::from(expected)),
                    priority: false,
                })
            );
            assert!(!planner.holds_peek());
            assert_eq!(
                *log.lock().unwrap(),
                [format!("peek {expected}")],
                "no commit before send"
            );
            planner.commit_after_send(&mut sched);
            planner.record_sent(&plan.screen());
        }
        // The same holds with pairing off, an unpublished canvas (`None`), or
        // a non-pairable P.
        let (mut sched, log) = Scripted::new("a b");
        let mut planner = ScreenPlanner::new(1.0, Some(3));
        let plan = planner.plan(&mut sched, None).unwrap();
        assert!(matches!(plan, Plan::SendThenCommit(_)));
        assert_eq!(*log.lock().unwrap(), ["peek a"]);
        let none = |_: &Path| false;
        planner.commit_after_send(&mut sched);
        let plan = planner.plan(&mut sched, Some(&none)).unwrap();
        assert!(matches!(plan, Plan::SendThenCommit(_)));
        assert!(!planner.holds_peek());
    }

    /// **S7 (B5).** With a seed, the coin's first draw differs from the
    /// scheduler's first draw; without one, both are OS-seeded.
    #[test]
    fn s7_coin_stream_is_not_the_schedulers() {
        for seed in 0..256u64 {
            let scheduler_first = StdRng::seed_from_u64(seed).random::<f64>();
            let coin_first = coin_rng(Some(seed)).random::<f64>();
            assert_ne!(scheduler_first, coin_first, "seed {seed}");
            // Deterministic per seed.
            assert_eq!(coin_first, coin_rng(Some(seed)).random::<f64>());
        }
        let a = coin_rng(None).random::<u64>();
        let b = coin_rng(None).random::<u64>();
        assert_ne!(a, b, "OS-seeded coins differ");
    }

    #[test]
    fn splitmix64_matches_the_reference_values() {
        // First outputs of the reference splitmix64 generator seeded with 0
        // (state advances by the golden gamma before mixing).
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix64(0x9E37_79B9_7F4A_7C15), 0x6E78_9E6A_A1B9_65F4);
    }

    #[test]
    fn the_coin_is_skipped_at_p1_and_thins_pairs_below() {
        let seq = (0..400)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let pairs = |p: f64| {
            plan_all(&seq, &all, p)
                .iter()
                .filter(|s| s.contains('+'))
                .count()
        };
        assert_eq!(pairs(1.0), 200);
        let half = pairs(0.5);
        assert!((90..=160).contains(&half), "{half}");
    }

    // ── Look-ahead (design "Look-ahead pairing", 2026-10-22) ────────────────

    /// Plan every screen of `seq` with a window of `w`, sending each at once.
    fn plan_all_w(seq: &str, pairable: &dyn Fn(&Path) -> bool, p: f64, w: usize) -> Vec<String> {
        plan_all_w_logged(seq, pairable, p, w, 1).0
    }

    fn plan_all_w_logged(
        seq: &str,
        pairable: &dyn Fn(&Path) -> bool,
        p: f64,
        w: usize,
        seed: u64,
    ) -> (Vec<String>, Vec<String>) {
        let (mut sched, log) = Scripted::new(seq);
        let mut planner = ScreenPlanner::new(p, Some(seed)).with_lookahead(w);
        let mut out = Vec::new();
        while let Some(plan) = planner.plan(&mut sched, Some(pairable)) {
            let screen = plan.screen();
            if let Plan::SendThenCommit(_) = plan {
                planner.commit_after_send(&mut sched);
            }
            planner.record_sent(&screen);
            out.push(names(&screen));
        }
        let log = std::mem::take(&mut *log.lock().unwrap());
        (out, log)
    }

    fn not_l(p: &Path) -> bool {
        !p.to_str().unwrap().starts_with('L')
    }

    /// A pairable head pairs with the first pairable portrait within W
    /// positions; the photos between are each shifted one appearance later.
    #[test]
    fn lookahead_pairs_across_a_gap_and_shifts_the_photos_between() {
        assert_eq!(
            plan_all_w("A L1 L2 B C", &not_l, 1.0, 3),
            ["A+B", "L1", "L2", "C"]
        );
        // W = 2 cannot reach B at position 3; B then pairs back to back with C.
        assert_eq!(
            plan_all_w("A L1 L2 B C", &not_l, 1.0, 2),
            ["A", "L1", "L2", "B+C"]
        );
        // The shifted photos are the next screens' heads, in order, and can
        // pair themselves (C with D across L2).
        assert_eq!(
            plan_all_w("A L1 C L2 D B E", &not_l, 1.0, 3),
            ["A+C", "L1", "L2", "D+B", "E"]
        );
    }

    /// Every commit happens in sequence order (P1-sched): looking past a
    /// landscape commits it, so the scheduler sees exactly the unpaired run.
    #[test]
    fn lookahead_commits_only_in_sequence_order() {
        let (screens, log) = plan_all_w_logged("A L1 L2 B C", &not_l, 1.0, 3, 1);
        assert_eq!(screens, ["A+B", "L1", "L2", "C"]);
        assert_eq!(
            log,
            [
                "peek A",
                "commit A",
                "peek L1",
                "commit L1",
                "peek L2",
                "commit L2",
                "peek B",
                "commit B",
                "peek C", // (A, B) planned; L1, L2 queued, C held
                "commit C",
                "peek -", // L1 and L2 solo need no scheduler call
                "peek -", // the final, empty plan
            ]
        );
    }

    /// I5 under a shift: pulling B out of A, L, B, L would send L, L. The pair
    /// is refused and L never lands next to its own other appearance.
    #[test]
    fn lookahead_refuses_a_shift_that_puts_a_photo_next_to_itself() {
        assert_eq!(
            plan_all_w("A L B L C", &not_l, 1.0, 3),
            ["A", "L", "B+C", "L"]
        );
        // X₁ = X_k: pairing A with the second B would put B on consecutive
        // screens ((A, B), (B, ...)); refused.
        assert_eq!(
            plan_all_w("A B A B C", &all, 1.0, 3),
            ["A", "B+C", "A", "B"]
        );
        // X₁ = A (a debut's Z, Z): no pull over it; the second A then pairs
        // back to back with B exactly as rc2 does. The repeat is the
        // sequence's own, not made worse.
        assert_eq!(plan_all_w("A A B", &all, 1.0, 3), ["A", "A+B"]);
        assert_eq!(plan_all_w("A A B", &all, 1.0, 1), ["A", "A+B"]);
    }

    /// Random short sequences over a small alphabet (so repeats occur): the
    /// sent order is a permutation of the sequence, every appearance moves at
    /// most W − 1 places, and consecutive-screen repeats are never more than
    /// the sequence's own Z, Z adjacencies (I5).
    #[test]
    fn lookahead_random_sequences_keep_i5_and_the_move_bound() {
        let alphabet = ["a", "b", "c", "d", "L1", "L2"];
        let mut rng = StdRng::seed_from_u64(42);
        let mut pairs_seen = 0;
        for case in 0..400u64 {
            let seq: Vec<&str> = (0..60)
                .map(|_| alphabet[rng.random_range(0..alphabet.len())])
                .collect();
            let text = seq.join(" ");
            let own_zz = seq.windows(2).filter(|w| w[0] == w[1]).count();
            for w in [2usize, 3, 4] {
                for p in [1.0, 0.5] {
                    let (screens, _) = plan_all_w_logged(&text, &not_l, p, w, case);
                    let planned: Vec<PathBuf> = screens
                        .iter()
                        .flat_map(|s| s.split('+'))
                        .map(PathBuf::from)
                        .collect();
                    let unpaired: Vec<PathBuf> = seq.iter().map(PathBuf::from).collect();
                    let mut a = planned.clone();
                    let mut b = unpaired.clone();
                    a.sort();
                    b.sort();
                    assert_eq!(a, b, "case {case} W {w}: not a permutation");
                    let moved = Displacement::measure(&planned, &unpaired);
                    assert_eq!(moved.unmatched, 0);
                    assert!(moved.max < w, "case {case} W {w}: moved {}", moved.max);
                    let sent_zz = planned.windows(2).filter(|w| w[0] == w[1]).count();
                    assert!(
                        sent_zz <= own_zz,
                        "case {case} W {w}: {sent_zz} Z,Z sent vs {own_zz} in the sequence\n{text}\n{screens:?}"
                    );
                    // Every consecutive-screen repeat is a sent Z, Z.
                    for pair in screens.windows(2) {
                        let (prev, next): (Vec<&str>, Vec<&str>) =
                            (pair[0].split('+').collect(), pair[1].split('+').collect());
                        for z in next.iter().filter(|z| prev.contains(z)) {
                            assert!(
                                prev.last() == Some(z) && next.first() == Some(z),
                                "case {case} W {w}: {:?} then {:?}",
                                pair[0],
                                pair[1]
                            );
                        }
                    }
                    pairs_seen += screens.iter().filter(|s| s.contains('+')).count();
                }
            }
        }
        assert!(pairs_seen > 1000);
    }

    /// rc2's planner (steps 1–5 with a single C, as shipped in v1.2.0-rc2),
    /// the oracle for "W ≤ 1 is exactly today's planner".
    struct Rc2 {
        committed: Option<Appearance>,
        held: Option<Appearance>,
        previous: Vec<Arc<PathBuf>>,
        probability: f64,
        coin: StdRng,
    }

    impl Rc2 {
        fn new(probability: f64, seed: Option<u64>) -> Self {
            Self {
                committed: None,
                held: None,
                previous: Vec::new(),
                probability,
                coin: coin_rng(seed),
            }
        }

        fn plan(
            &mut self,
            sched: &mut dyn PlaylistScheduler,
            pairable: Option<&dyn Fn(&Path) -> bool>,
        ) -> Option<Plan> {
            let (first, first_committed) = if let Some(c) = self.committed.take() {
                (c, true)
            } else if let Some(h) = self.held.take() {
                (h, false)
            } else {
                (peek(sched)?, false)
            };
            let pairable = match pairable {
                Some(pairable) if self.probability > 0.0 && pairable(&first.path) => pairable,
                _ => {
                    return Some(if first_committed {
                        Plan::Committed(PlannedScreen::solo(first))
                    } else {
                        Plan::SendThenCommit(first)
                    });
                }
            };
            if !first_committed {
                sched.commit_shown();
            }
            let next = match self.held.take() {
                Some(held) => held,
                None => match peek(sched) {
                    Some(next) => next,
                    None => return Some(Plan::Committed(PlannedScreen::solo(first))),
                },
            };
            let pair_conditions = pairable(&next.path)
                && next.path != first.path
                && !self.previous.contains(&next.path)
                && (self.probability >= 1.0 || self.coin.random::<f64>() < self.probability);
            if !pair_conditions {
                self.held = Some(next);
                return Some(Plan::Committed(PlannedScreen::solo(first)));
            }
            sched.commit_shown();
            self.held = peek(sched);
            if self
                .held
                .as_ref()
                .is_some_and(|after| after.path == first.path)
            {
                self.committed = Some(next);
                return Some(Plan::Committed(PlannedScreen::solo(first)));
            }
            Some(Plan::Committed(PlannedScreen {
                primary: first,
                partner: Some(next),
            }))
        }
    }

    /// **W ≤ 1 equals today's planner exactly:** screens and the scheduler's
    /// peek/commit log agree with the rc2 oracle on random sequences, for
    /// p = 1 and p = 0.5 (the coin stream is drawn identically).
    #[test]
    fn w1_and_w0_equal_todays_planner_exactly() {
        let alphabet = ["a", "b", "c", "d", "L1", "L2"];
        let mut rng = StdRng::seed_from_u64(7);
        for case in 0..300u64 {
            let text = (0..40)
                .map(|_| alphabet[rng.random_range(0..alphabet.len())])
                .collect::<Vec<_>>()
                .join(" ");
            for p in [1.0, 0.5] {
                let (mut sched, log) = Scripted::new(&text);
                let mut oracle = Rc2::new(p, Some(case));
                let mut expected = Vec::new();
                while let Some(plan) = oracle.plan(&mut sched, Some(&not_l)) {
                    let screen = plan.screen();
                    if let Plan::SendThenCommit(_) = plan {
                        sched.commit_shown();
                    }
                    oracle.previous = screen.members().map(|m| Arc::clone(&m.path)).collect();
                    expected.push(names(&screen));
                }
                let expected_log = std::mem::take(&mut *log.lock().unwrap());
                for w in [0usize, 1] {
                    let (screens, log) = plan_all_w_logged(&text, &not_l, p, w, case);
                    assert_eq!(screens, expected, "case {case} p {p} W {w}");
                    assert_eq!(log, expected_log, "case {case} p {p} W {w}");
                }
            }
        }
    }

    /// Rate model: on independent draws the paired share of pairable
    /// appearances is 2q/(1+q), q = 1 − (1 − πp)^W. At π = 0.34 (the Pi
    /// library) W = 3 lifts it from 0.51 to 0.83.
    #[test]
    fn lookahead_rate_matches_2q_over_1_plus_q_on_independent_draws() {
        let pi = 0.34;
        assert!((ScreenStats::predicted_share_lookahead(pi, 1.0, 1) - 0.507).abs() < 0.001);
        assert!((ScreenStats::predicted_share_lookahead(pi, 1.0, 3) - 0.832).abs() < 0.001);
        assert_eq!(
            ScreenStats::predicted_share_lookahead(pi, 0.5, 1),
            ScreenStats::predicted_share(pi, 0.5)
        );
        let mut rng = StdRng::seed_from_u64(3);
        let kinds: Vec<bool> = (0..30_000).map(|_| rng.random::<f64>() < pi).collect();
        let text = (0..kinds.len())
            .map(|i| {
                if kinds[i] {
                    format!("p{i}")
                } else {
                    format!("L{i}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        for (w, p) in [(1usize, 1.0), (3, 1.0), (3, 0.5), (5, 1.0)] {
            let (screens, _) = plan_all_w_logged(&text, &not_l, p, w, 1);
            let pairs = screens.iter().filter(|s| s.contains('+')).count();
            let pairable = kinds.iter().filter(|k| **k).count();
            let measured = 2.0 * pairs as f64 / pairable as f64;
            let predicted = ScreenStats::predicted_share_lookahead(pi, p, w);
            assert!(
                (measured - predicted).abs() < 0.02,
                "W {w} p {p}: measured {measured:.3} vs {predicted:.3}"
            );
        }
    }

    /// The dry run's `# pairing` block reports the W-aware rate and the move
    /// bound with look-ahead on, and rc2's line with it off.
    #[test]
    fn report_lines_carry_the_lookahead_rate_and_move_bound() {
        let (screens, _) = plan_all_w_logged("A L1 L2 B C", &not_l, 1.0, 3, 1);
        let planned: Vec<PlannedScreen> = screens
            .iter()
            .map(|s| {
                let mut members = s.split('+').map(|n| Appearance {
                    path: Arc::new(PathBuf::from(n)),
                    priority: false,
                });
                PlannedScreen {
                    primary: members.next().unwrap(),
                    partner: members.next(),
                }
            })
            .collect();
        let stats = ScreenStats::measure(&planned, &not_l);
        let flat: Vec<PathBuf> = planned
            .iter()
            .flat_map(PlannedScreen::members)
            .map(|m| (*m.path).clone())
            .collect();
        let unpaired: Vec<PathBuf> = "A L1 L2 B C".split(' ').map(PathBuf::from).collect();
        let moved = Displacement::measure(&flat, &unpaired);
        assert_eq!(
            moved,
            Displacement {
                max: 2,
                unmatched: 0
            }
        );
        let lines = stats.report_lines(0.34, 1.0, 3, moved);
        let text = lines.join("\n");
        assert!(text.contains("q = 1-(1-pi*p)^W = 0.832 at W = 3"), "{text}");
        assert!(text.contains("back to back would give 0.507"), "{text}");
        assert!(
            text.contains(
                "max shift from the unpaired order: 2 appearances (bound W-1 = 2; 0 unmatched"
            ),
            "{text}"
        );
        assert!(text.contains("consecutive-screen repeats: 0"), "{text}");
        let off = stats
            .report_lines(0.34, 1.0, 1, Displacement::default())
            .join("\n");
        assert!(
            off.contains("2*pi*p/(1+pi*p) = 0.507, an approximation"),
            "{off}"
        );
        assert!(
            off.contains("max shift from the unpaired order: 0 appearances (bound W-1 = 0;"),
            "{off}"
        );
    }

    /// Removal (B4) drops a queued, shifted photo and keeps the rest in order.
    #[test]
    fn a_removed_queued_photo_is_dropped_from_the_shifted_queue() {
        let (mut sched, _) = Scripted::new("A L1 L2 B C");
        let mut planner = ScreenPlanner::new(1.0, Some(1)).with_lookahead(3);
        let plan = planner.plan(&mut sched, Some(&not_l)).unwrap();
        assert_eq!(names(&plan.screen()), "A+B");
        assert_eq!(planner.queued(), ["L1", "L2"]);
        planner.record_sent(&plan.screen());
        planner.on_removed(Path::new("L1"));
        assert_eq!(planner.queued(), ["L2"]);
        assert!(!planner.holds_peek(), "a removal re-peeks H");
        let plan = planner.plan(&mut sched, Some(&not_l)).unwrap();
        assert_eq!(names(&plan.screen()), "L2");
        assert!(matches!(plan, Plan::Committed(_)));
    }
}
