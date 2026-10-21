//! The screen planner: back-to-back portrait pairing as a pure function of
//! the scheduler (`local-markdown/p1-pairing-design.md` §2).
//!
//! The scheduler is not changed at all. The planner only groups consecutive
//! appearances of the sequence it already produces: **when the scheduler's
//! next two appearances are both pairable, they share one screen.** Between
//! screens it keeps at most two things: `C`, one committed but unsent
//! appearance, and `H`, one held peek (not committed). Each step below is
//! named as in the design.
//!
//! Invariants (proofs in the design):
//! - **P1, the appearance sequence is unchanged.** With the same seed and no
//!   `PhotoAdded`/`PhotoRemoved`, the flattened sequence of planned screens
//!   is identical to the unpaired scheduler's, for every `p` and library:
//!   scheduler peeks and commits happen only in sequence order, a held peek
//!   is never redrawn, and the coin has its own stream.
//! - **I5, no photo on consecutive screens** except where the unpaired
//!   sequence already repeats (`Z, Z`): rules 4 and 5.
//!
//! Both are exact here and in `simulate_screens` / the dry run. The live
//! loop drives the same planner, but today's re-peeks during a blocked send
//! and inventory events between planning and display make the live sequence
//! match in distribution only (design B7, V1).

use super::PlaylistScheduler;
use rand::{Rng, SeedableRng, rngs::StdRng};
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
    /// `C`: committed but unsent; always sent on the next screen.
    committed: Option<Appearance>,
    /// `H`: the held peek (not committed).
    held: Option<Appearance>,
    /// **"Previous screen" (rule 4, V2):** the members of the last screen
    /// actually sent, recorded for every screen including today's-path solos
    /// (after a B4 degrade it is the degraded screen; a dropped empty screen
    /// leaves it unchanged).
    previous: Vec<Arc<PathBuf>>,
    /// `p`, `pairing.probability`.
    probability: f64,
    coin: StdRng,
}

impl ScreenPlanner {
    pub fn new(probability: f64, seed: Option<u64>) -> Self {
        Self {
            committed: None,
            held: None,
            previous: Vec::new(),
            probability,
            coin: coin_rng(seed),
        }
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
        // Step 1. First member: P is C if present, else H if present, else a
        // fresh peek.
        let (first, first_committed) = if let Some(c) = self.committed.take() {
            (c, true)
        } else if let Some(h) = self.held.take() {
            (h, false)
        } else {
            (peek(sched)?, false)
        };

        // Step 2. Today's path: P alone if pairing is off, p = 0, the canvas is
        // not yet published (`pairable` is None), or P is not pairable. A P
        // from a peek is sent, then committed (B9); a C is already committed.
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

        // Step 3. Look one ahead: commit P, then X is H (peeked after P was
        // committed) or a fresh peek, and becomes H.
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

        // Step 4. Pair conditions (B1-a): X pairable, X ≠ P, X not on the
        // previous screen (rule 4), and the pairing coin. Otherwise P alone,
        // X stays held.
        let pair_conditions = pairable(&next.path)
            && next.path != first.path
            && !self.previous.contains(&next.path)
            && self.coin_succeeds();
        if !pair_conditions {
            self.held = Some(next);
            return Some(Plan::Committed(PlannedScreen::solo(first)));
        }

        // Step 5. Look past X (rule 5, B1-b): commit X and peek Y, which
        // becomes H. If Y = P, P goes alone and X becomes C (sent as the next
        // screen's first member); otherwise the screen is (P, X).
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
    pub fn invalidate_held(&mut self) {
        self.held = None;
    }

    /// **Removal (step 7, B4):** a removed photo held as C is dropped; the
    /// surviving appearances keep their order. (The caller degrades a
    /// planned, unsent screen with [`PlannedScreen::without`].)
    pub fn on_removed(&mut self, path: &Path) {
        if self
            .committed
            .as_ref()
            .is_some_and(|c| c.path.as_path() == path)
        {
            self.committed = None;
        }
        self.invalidate_held();
    }

    /// Whether a held peek is outstanding (for tests of B7/B9).
    #[cfg(test)]
    pub fn holds_peek(&self) -> bool {
        self.held.is_some()
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
        2.0 * pi * probability / (1.0 + pi * probability)
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
}
