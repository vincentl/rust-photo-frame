use crate::config::PlaylistOptions;
use crate::events::{CreatedSource, Displayed, InventoryEvent, LoadPartner, LoadPhoto, PhotoInfo};
use anyhow::Result;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub mod pairing;
pub mod planner;
use pairing::{DisplaySizes, LivePairing, PairingRules};
use planner::{Plan, PlannedScreen, ScreenPlanner};

/// Orchestrates the playlist via a virtual-time min-heap scheduler.
///
/// Rules:
/// - Each photo has a scheduling key drawn from an exponential gap distribution
///   inversely proportional to its weight. Higher weight ⇒ smaller mean gap ⇒ shown more often.
/// - The photo with the smallest key is always shown next.
/// - On show, the photo is rescheduled at vclock + new gap (no rebuild needed).
/// - `PhotoAdded` / `PhotoRemoved` are O(log n) heap ops; removed entries are lazily skipped.
///
/// The scheduling algorithm is pluggable via [`PlaylistScheduler`]; the concrete
/// implementation is selected from `playlist.order`. New algorithms (e.g. a
/// shuffle-bag) can be added without touching the manager loop.
pub trait PlaylistScheduler {
    fn record_add(&mut self, info: PhotoInfo);
    fn record_remove(&mut self, path: &Path);
    /// Front photo and its priority (`true` until first shown), without committing.
    fn peek_next(&mut self) -> Option<(Arc<PathBuf>, bool)>;
    /// Advance past the photo `peek_next` returned (mark shown, reschedule).
    fn commit_shown(&mut self);
    /// Photos currently in inventory (for `photo_display_metric`).
    fn inventory_len(&self) -> usize;
    /// Current scheduling weight for a known path (for `photo_display_metric`).
    fn current_weight(&self, path: &Path) -> Option<f64>;
    /// Age in seconds (now − `created_at`) for a known path (for `photo_display_metric`).
    fn age_seconds(&self, path: &Path) -> Option<f64>;
    /// Where a known path's `created_at` came from (for `photo_display_metric`).
    fn created_source(&self, path: &Path) -> Option<CreatedSource>;
}

/// Build the scheduler selected by `playlist.order`.
pub fn build_scheduler(
    options: PlaylistOptions,
    seed_override: Option<u64>,
    now_override: Option<SystemTime>,
) -> Box<dyn PlaylistScheduler + Send> {
    let rng = match seed_override {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_os_rng(),
    };
    // weighted-random and weighted-spread share the virtual-timeline heap; they
    // differ only in the refractory floor captured by `refractory_fraction`.
    Box::new(PlaylistState::with_rng(options, rng, now_override))
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    mut displayed_rx: Receiver<Displayed>,
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
    options: PlaylistOptions,
    now_override: Option<SystemTime>,
    seed_override: Option<u64>,
    metrics: bool,
    pairing: Option<LivePairing>,
) -> Result<()> {
    if metrics {
        log_scheduler_stamp(&options, pairing.as_ref().map(|p| p.rules.lookahead));
    }
    let playlist = build_scheduler(options, seed_override, now_override);
    let mut screens = LiveScreens::new(playlist, pairing, seed_override);
    let mut display_log = DisplayLog::default();
    let mut canvas_version_seen = 0;

    loop {
        screens.log_canvas_change(&mut canvas_version_seen);
        let next = screens.next_request();
        let next_is_some = next.is_some();

        select! {
            _ = cancel.cancelled() => break,

            // Drive slideshow by sending the next screen to the loader. Today's
            // path commits (pop + reschedule) only after a successful send so no
            // slide is lost; a committed screen was committed when planned.
            res = {
                let to_loader = to_loader.clone();
                async move {
                    match next {
                        Some(request) => to_loader.send(request).await.map_err(|_| ()),
                        None => Err(()),
                    }
                }
            }, if next_is_some => {
                match res {
                    Ok(()) => screens.sent(),
                    Err(()) => {
                        warn!("loader channel closed");
                        break;
                    }
                }
            }

            // Inventory updates (from files task)
            maybe_ev = inv_rx.recv() => {
                if let Some(event) = maybe_ev {
                    screens.on_inventory(event);
                }
            },

            // Displayed notifications (informational only)
            maybe_disp = displayed_rx.recv() => {
                if let Some(Displayed { path, partner }) = maybe_disp {
                    let evidence = screens.on_displayed(&path);
                    if metrics {
                        display_log.log_screen(&path, partner.as_deref(), evidence, &screens);
                    } else if let Some(partner) = partner {
                        debug!("displayed: {} + {}", path.display(), partner.display());
                    } else {
                        debug!("displayed: {}", path.display());
                    }
                }
            }

            // Idle tick: prevents spinning when the heap is empty at startup.
            _ = sleep(Duration::from_millis(50)) => {}
        }
    }

    Ok(())
}

/// Stamp the scheme + parameters once per process so a grepped
/// photo_display_metric series can be interpreted (and segmented across
/// sessions / config changes) without guessing what produced it. With
/// portrait pairing off the line is exactly the line before pairing; with it
/// on (`pairing` = the look-ahead window W, 1 for back to back), the pairing
/// parameters follow (as configured, e.g. `0.618`).
fn log_scheduler_stamp(options: &PlaylistOptions, pairing: Option<usize>) {
    if let Some(lookahead) = pairing {
        info!(
            order = %options.order,
            refractory = format_args!("{:.3}", options.refractory_fraction()),
            min_spacing = format_args!("{:.3}", options.min_spacing),
            new_multiplicity = options.new_multiplicity,
            half_life_secs = options.half_life.as_secs(),
            pairing = true,
            pairing_probability = format_args!("{}", options.pairing.probability),
            pairing_lookahead = lookahead,
            pairing_bar_ratio = format_args!("{}", options.pairing.bar_ratio),
            pairing_dwell_multiplier = format_args!("{}", options.pairing.dwell_multiplier),
            "playlist_scheduler"
        );
    } else {
        info!(
            order = %options.order,
            refractory = format_args!("{:.3}", options.refractory_fraction()),
            min_spacing = format_args!("{:.3}", options.min_spacing),
            new_multiplicity = options.new_multiplicity,
            half_life_secs = options.half_life.as_secs(),
            "playlist_scheduler"
        );
    }
}

/// The live loop's screen state: the scheduler, the displayed sizes, the
/// screen planner and the planned screen not yet sent. With pairing off
/// every plan is today's path (peek, send, commit after the send, re-peek on
/// every iteration), so the loop is exactly the loop before pairing.
struct LiveScreens {
    playlist: Box<dyn PlaylistScheduler + Send>,
    /// Displayed sizes under the one-authority rule (`pairing`).
    sizes: DisplaySizes,
    planner: ScreenPlanner,
    planned: Option<Plan>,
    pairing: Option<LivePairing>,
    /// Screens sent so far; the latest ones with their members, to tell
    /// which sent screen a `Displayed` is (metrics only).
    sent_count: u64,
    recent_sends: VecDeque<(u64, Vec<PathBuf>)>,
    /// `PhotoRemoved` / `PhotoAdded` events since the last `Displayed`
    /// (metrics only).
    removed_since_display: u64,
    added_since_display: u64,
}

/// What the manager knows about a displayed screen's history, logged so the
/// analyzer can attribute a photo on consecutive screens to its cause
/// (design V1): a send index that jumps means a screen was skipped (failed
/// decode, purge, sleep reset); adds and removals mean an inventory change
/// between planning and display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DisplayEvidence {
    /// 1-based index of the sent screen this display is, `None` if unknown.
    sent: Option<u64>,
    /// `PhotoRemoved` events since the previous display.
    removed: u64,
    /// `PhotoAdded` events since the previous display (an add re-peeks the
    /// held photo, which can redraw the debut coin).
    added: u64,
}

/// How many sent screens are remembered to match displays against (well
/// beyond the loader and viewer queues).
const RECENT_SENDS: usize = 64;

impl LiveScreens {
    fn new(
        playlist: Box<dyn PlaylistScheduler + Send>,
        pairing: Option<LivePairing>,
        seed: Option<u64>,
    ) -> Self {
        let probability = pairing.as_ref().map_or(0.0, |p| p.rules.probability);
        let lookahead = pairing.as_ref().map_or(1, |p| p.rules.lookahead);
        Self {
            playlist,
            sizes: DisplaySizes::default(),
            planner: ScreenPlanner::new(probability, seed).with_lookahead(lookahead),
            planned: None,
            pairing,
            sent_count: 0,
            recent_sends: VecDeque::with_capacity(RECENT_SENDS),
            removed_since_display: 0,
            added_since_display: 0,
        }
    }

    /// Pairability now (read at each decision): the rule on the recorded
    /// size and the published canvas.
    fn is_pairable(&self, path: &Path) -> bool {
        self.pairing
            .as_ref()
            .is_some_and(|p| p.rules.is_pairable(self.sizes.get(path), p.canvas.get()))
    }

    /// The request for the next screen, planning it if needed. Today's path
    /// re-peeks on every iteration, as the loop always has (a debut arriving
    /// meanwhile can take the slot); a committed screen (**commit before
    /// send**, pairing on with p > 0) stays planned until it is sent.
    fn next_request(&mut self) -> Option<LoadPhoto> {
        if !matches!(self.planned, Some(Plan::Committed(_))) {
            let Self {
                playlist,
                sizes,
                planner,
                planned,
                pairing,
                ..
            } = self;
            let canvas = pairing.as_ref().and_then(|p| p.canvas.get());
            let is_pairable = |path: &Path| {
                pairing
                    .as_ref()
                    .is_some_and(|p| p.rules.is_pairable(sizes.get(path), canvas))
            };
            // `None` (today's path) while pairing is off or the canvas is
            // not yet published.
            let pairable: Option<&dyn Fn(&Path) -> bool> =
                canvas.is_some().then_some(&is_pairable as _);
            *planned = planner.plan(playlist.as_mut(), pairable);
        }
        self.planned
            .as_ref()
            .map(|plan| load_request(&plan.screen()))
    }

    /// The planned screen reached the loader.
    fn sent(&mut self) {
        let plan = self.planned.take().expect("a planned screen was sent");
        if let Plan::SendThenCommit(_) = plan {
            self.planner.commit_after_send(self.playlist.as_mut());
        }
        let screen = plan.screen();
        self.planner.record_sent(&screen);
        self.sent_count += 1;
        if self.recent_sends.len() == RECENT_SENDS {
            self.recent_sends.pop_front();
        }
        let members = screen.members().map(|m| (*m.path).clone()).collect();
        self.recent_sends.push_back((self.sent_count, members));
    }

    /// A screen led by `path` was displayed: which send it was (the latest
    /// one containing `path`; a pair the loader degraded is led by its other
    /// member), and the removals since the previous display.
    fn on_displayed(&mut self, path: &Path) -> DisplayEvidence {
        let sent = self
            .recent_sends
            .iter()
            .rev()
            .find(|(_, members)| members.iter().any(|m| m == path))
            .map(|(index, _)| *index);
        DisplayEvidence {
            sent,
            removed: std::mem::take(&mut self.removed_since_display),
            added: std::mem::take(&mut self.added_since_display),
        }
    }

    fn on_inventory(&mut self, event: InventoryEvent) {
        match event {
            InventoryEvent::PhotoAdded(info) => {
                self.added_since_display += 1;
                self.sizes.on_added(&info);
                self.playlist.record_add(info);
                // B7: a held peek is re-peeked after an add.
                self.planner.invalidate_held();
            }
            InventoryEvent::PhotoRemoved(path) => {
                self.removed_since_display += 1;
                self.sizes.on_removed(&path);
                self.playlist.record_remove(&path);
                // B4/B7: a removed C is dropped, and a held peek re-peeked.
                self.planner.on_removed(&path);
                // B4: a screen carries only photos in the inventory at send
                // time. A removed member of the planned, unsent screen is
                // dropped (a pair degrades to its other member); an emptied
                // screen is dropped and the next one is planned.
                if let Some(Plan::Committed(screen)) = self.planned.take() {
                    self.planned = screen.without(&path).map(Plan::Committed);
                }
            }
            // B7: PhotoMeta (like CanvasShape) changes no scheduling state;
            // the held peek stays.
            InventoryEvent::PhotoMeta {
                path,
                display_size,
                source,
            } => {
                self.sizes.on_meta(&path, display_size, source);
            }
        }
    }

    /// Log the published canvas and how much of the inventory is pairable
    /// on it, once per canvas version (`pairing_canvas`).
    fn log_canvas_change(&self, seen: &mut u64) {
        let Some(pairing) = &self.pairing else {
            return;
        };
        let version = pairing.canvas.version();
        if version == *seen {
            return;
        }
        *seen = version;
        let canvas = pairing.canvas.get();
        let (mut known, mut pairable, mut total) = (0usize, 0usize, 0usize);
        for (_, size) in self.sizes.iter() {
            total += 1;
            known += usize::from(size.is_some());
            pairable += usize::from(pairing.rules.is_pairable(size, canvas));
        }
        info!(
            canvas = ?canvas,
            inventory = total,
            size_known = known,
            pairable,
            "pairing_canvas"
        );
    }
}

/// The loader request for a planned screen.
fn load_request(screen: &PlannedScreen) -> LoadPhoto {
    LoadPhoto {
        path: (*screen.primary.path).clone(),
        priority: screen.primary.priority,
        partner: screen.partner.as_ref().map(|partner| LoadPartner {
            path: (*partner.path).clone(),
            priority: partner.priority,
        }),
    }
}

struct PlaylistState {
    heap: BinaryHeap<Entry>,
    known: HashMap<PathBuf, Meta>,
    /// Generation counter per path, persisted across removals to invalidate stale heap entries.
    generations: HashMap<PathBuf, u32>,
    vclock: f64,
    seq: u64,
    rng: StdRng,
    options: PlaylistOptions,
    /// Refractory floor `f` for the scheduling gap, as a fraction of the mean
    /// interval (`0.0` = exponential/weighted-random; up to ~0.95 for
    /// weighted-spread). Cached from `options`.
    refractory: f64,
    /// Photos discovered at runtime, awaiting their prompt first show. Drained
    /// ahead of the heap by a biased coin (see `peek_next`) so a freshly added
    /// album debuts within seconds instead of waiting out the refractory floor.
    priority_fifo: VecDeque<Arc<PathBuf>>,
    /// Whether the most recent `peek_next` chose the FIFO front (so `commit_shown`
    /// pops the matching source).
    next_from_fifo: bool,
    now_override: Option<SystemTime>,
}

struct Meta {
    created_at: SystemTime,
    created_source: CreatedSource,
    generation: u32,
    shown: bool,
}

struct Entry {
    key: f64,
    seq: u64,
    generation: u32,
    path: Arc<PathBuf>,
}

// BinaryHeap is a max-heap; invert key comparison so the smallest key is popped first.
// Tiebreak by smaller seq (earlier insertion) for deterministic ordering.
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .key
            .total_cmp(&self.key)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl PartialEq for Entry {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key && self.seq == o.seq
    }
}
impl Eq for Entry {}

impl PlaylistState {
    fn with_rng(options: PlaylistOptions, rng: StdRng, now_override: Option<SystemTime>) -> Self {
        let refractory = options.refractory_fraction();
        Self {
            heap: BinaryHeap::new(),
            known: HashMap::new(),
            generations: HashMap::new(),
            vclock: 0.0,
            seq: 0,
            rng,
            options,
            refractory,
            priority_fifo: VecDeque::new(),
            next_from_fifo: false,
            now_override,
        }
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    /// Scheduling gap with mean `1/weight`, drawn from a refractory (dead-time)
    /// renewal law: a deterministic floor `f/weight` that no showing may fall
    /// below, plus exponential jitter with mean `(1-f)/weight` above it. The
    /// total mean is `1/weight` for every `f`, so the weighting (long-run show
    /// frequency) is unaffected; `f` only controls spacing, with coefficient of
    /// variation `1 - f`. `f == 0` is the original memoryless exponential gap
    /// (weighted-random); larger `f` guarantees a minimum gap before a photo
    /// can recur (weighted-spread).
    fn sample_gap(&mut self, weight: f64) -> f64 {
        let mean = 1.0 / weight.max(1.0);
        let f = self.refractory;
        let u = 1.0 - self.rng.random::<f64>(); // random::<f64>() ∈ [0,1), so u ∈ (0,1]
        f * mean + (1.0 - f) * mean * (-u.ln())
    }

    fn next_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    fn schedule(&mut self, path: Arc<PathBuf>, created_at: SystemTime, generation: u32) {
        let weight = self.options.weight_for(created_at, self.now());
        let key = self.vclock + self.sample_gap(weight);
        let seq = self.next_seq();
        self.heap.push(Entry {
            key,
            seq,
            generation,
            path,
        });
    }

    /// Reschedule the photo that was just shown. Unlike `schedule`, this
    /// guarantees the photo will not immediately reappear at the front of the
    /// queue while other photos are waiting: if its freshly sampled key would
    /// still be the smallest in the heap, it is pushed to just past the next
    /// photo with a fresh weighted gap. In the common case the sampled gap is
    /// already large enough and no adjustment is made, so the weighted cadence
    /// is preserved; only genuine back-to-back repeats are bumped. With a single
    /// photo the heap is empty here, so it is allowed to repeat — there is
    /// nothing else to show.
    fn reschedule_after_show(
        &mut self,
        path: Arc<PathBuf>,
        created_at: SystemTime,
        generation: u32,
    ) {
        let weight = self.options.weight_for(created_at, self.now());
        let mut key = self.vclock + self.sample_gap(weight);
        // Copy the next key out so the immutable heap borrow ends before we draw
        // another gap.
        if let Some(next_key) = self.heap.peek().map(|entry| entry.key)
            && key <= next_key
        {
            key = next_key + self.sample_gap(weight);
        }
        let seq = self.next_seq();
        self.heap.push(Entry {
            key,
            seq,
            generation,
            path,
        });
    }

    /// Drain leading tombstoned/stale entries off the heap and return the valid
    /// front path (without popping), or `None` if the heap holds nothing valid.
    fn valid_heap_front(&mut self) -> Option<Arc<PathBuf>> {
        loop {
            let (path, generation) = match self.heap.peek() {
                None => return None,
                Some(entry) => (entry.path.clone(), entry.generation),
            };
            if self
                .known
                .get(path.as_ref())
                .is_some_and(|m| m.generation == generation)
            {
                return Some(path);
            }
            self.heap.pop(); // tombstone / stale → drop
        }
    }
}

impl PlaylistScheduler for PlaylistState {
    fn record_add(&mut self, info: PhotoInfo) {
        // Already live (e.g. a metadata refresh): update created_at but keep the existing
        // schedule and generation — do not push another heap entry.
        if let Some(meta) = self.known.get_mut(&info.path) {
            meta.created_at = info.created_at;
            meta.created_source = info.created_source;
            return;
        }
        // New, or re-added after removal. Reading the bumped generation here ensures the
        // fresh heap entry has a strictly higher generation than any orphaned stale entries.
        let created_at = info.created_at;
        let created_source = info.created_source;
        let runtime_added = info.runtime_added;
        let path_arc = Arc::new(info.path);
        let generation = *self.generations.entry((*path_arc).clone()).or_insert(0);
        let weight = self.options.weight_for(created_at, self.now());
        self.known.insert(
            (*path_arc).clone(),
            Meta {
                created_at,
                created_source,
                generation,
                shown: false,
            },
        );
        debug!(path = %path_arc.display(), weight, "photo added to playlist");
        if runtime_added {
            // Debut promptly via the priority FIFO; it joins the heap timeline at
            // its first show (see `commit_shown`), so it isn't scheduled here.
            self.priority_fifo.push_back(path_arc);
        } else {
            self.schedule(path_arc, created_at, generation);
        }
    }

    fn record_remove(&mut self, path: &Path) {
        if self.known.remove(path).is_some() {
            // Bump generation so any outstanding heap entry for this path is treated as stale.
            // A future re-add will read this bumped value, making its entry valid again.
            if let Some(g) = self.generations.get_mut(path) {
                *g += 1;
            }
            debug!(path = %path.display(), "photo removed from playlist");
        }
    }

    /// Return the next photo to show and its priority (`!shown`), without
    /// committing. Newly added photos waiting in the priority FIFO are served
    /// ahead of the heap by a biased coin: the FIFO front wins with odds equal
    /// to its weight (`w : 1`), so a fresh photo debuts promptly while the rest
    /// of the rotation still shows ~`1/(w+1)` of the time. When only one source
    /// has a valid entry, that one is served. Returns `None` when both are empty.
    fn peek_next(&mut self) -> Option<(Arc<PathBuf>, bool)> {
        // Drop stale / already-shown entries from the front of the FIFO.
        while let Some(front) = self.priority_fifo.front() {
            if self.known.get(front.as_ref()).is_some_and(|m| !m.shown) {
                break;
            }
            self.priority_fifo.pop_front();
        }
        let heap_front = self.valid_heap_front();
        if let Some(fifo_front) = self.priority_fifo.front().cloned() {
            let serve_fifo = if heap_front.is_some() {
                let w = self.current_weight(fifo_front.as_ref()).unwrap_or(1.0);
                self.rng.random::<f64>() < w / (w + 1.0)
            } else {
                true
            };
            if serve_fifo {
                self.next_from_fifo = true;
                return Some((fifo_front, true));
            }
        }
        self.next_from_fifo = false;
        heap_front.map(|path| {
            let priority = !self.known[path.as_ref()].shown;
            (path, priority)
        })
    }

    /// Commit the photo `peek_next` just returned. A FIFO debut is popped, marked
    /// shown, and scheduled onto the heap for future (spaced) repeats *without*
    /// advancing vclock — the priority show is "extra" and must not disturb the
    /// heap timeline. A heap show advances vclock and reschedules as usual.
    fn commit_shown(&mut self) {
        if self.next_from_fifo {
            let Some(path) = self.priority_fifo.pop_front() else {
                return;
            };
            let (created_at, generation) = {
                let Some(meta) = self.known.get_mut(path.as_ref()) else {
                    return;
                };
                if meta.shown {
                    return; // already debuted (e.g. a duplicate FIFO entry)
                }
                meta.shown = true;
                (meta.created_at, meta.generation)
            };
            self.schedule(path, created_at, generation);
            return;
        }
        let entry = match self.heap.pop() {
            None => return,
            Some(e) => e,
        };
        let (created_at, generation) = {
            let Some(meta) = self.known.get_mut(entry.path.as_ref()) else {
                return;
            };
            if meta.generation != entry.generation {
                return;
            }
            meta.shown = true;
            (meta.created_at, meta.generation)
        };
        self.vclock = entry.key;
        self.reschedule_after_show(entry.path, created_at, generation);
    }

    /// Number of photos currently known (in inventory).
    fn inventory_len(&self) -> usize {
        self.known.len()
    }

    /// Current scheduling weight for a known path, or `None` if it has been
    /// removed from inventory since it was displayed.
    fn current_weight(&self, path: &Path) -> Option<f64> {
        self.known
            .get(path)
            .map(|meta| self.options.weight_for(meta.created_at, self.now()))
    }

    fn age_seconds(&self, path: &Path) -> Option<f64> {
        let now = self.now();
        self.known.get(path).map(|meta| {
            now.duration_since(meta.created_at)
                .unwrap_or_default()
                .as_secs_f64()
        })
    }

    fn created_source(&self, path: &Path) -> Option<CreatedSource> {
        self.known.get(path).map(|meta| meta.created_source)
    }
}

/// Per-photo display history used to emit `photo_display_metric` lines so the
/// randomness of the scheduler can be audited offline. Grep the lines out of
/// `journalctl -t photoframe`, list the photo directory recursively, and compare
/// the two datasets to find starved photos (never shown) or photos repeating
/// sooner than the inventory size would predict for uniform random.
#[derive(Default)]
struct DisplayLog {
    /// Total photos displayed so far (1-based `seq` is `total` after increment).
    total: u64,
    /// Screens displayed so far (a pair screen is one screen, two appearances).
    screens: u64,
    /// Per-path running history: (times shown, `seq` of the most recent show).
    history: HashMap<PathBuf, (u64, u64)>,
}

/// The screen a `photo_display_metric` line belongs to.
struct ScreenContext<'a> {
    playlist: &'a dyn PlaylistScheduler,
    /// Portrait pairing is on: the line carries the screen fields.
    pairing: bool,
    pairable: &'a dyn Fn(&Path) -> bool,
    evidence: DisplayEvidence,
}

/// A displayed photo's place on its screen, for `photo_display_metric`.
#[derive(Clone, Copy)]
enum ScreenRole {
    Solo,
    First,
    Second,
}

impl ScreenRole {
    fn as_str(self) -> &'static str {
        match self {
            ScreenRole::Solo => "solo",
            ScreenRole::First => "first",
            ScreenRole::Second => "second",
        }
    }
}

impl DisplayLog {
    /// One `photo_display_metric` line **per appearance**: since pairing
    /// leaves the appearance sequence unchanged (P1), `seq`, `gap` and every
    /// statistic mean what they meant before pairing. With pairing on, each
    /// line adds `screen=<n>`, `pair=solo|first|second`, `partner=`,
    /// `pairable=`, and the evidence `sent=`, `removed=`, `added=`; with it
    /// off the line is exactly the line before pairing.
    fn log_screen(
        &mut self,
        path: &Path,
        partner: Option<&Path>,
        evidence: DisplayEvidence,
        screens: &LiveScreens,
    ) {
        let screen = ScreenContext {
            playlist: screens.playlist.as_ref(),
            pairing: screens.pairing.is_some(),
            pairable: &|path: &Path| screens.is_pairable(path),
            evidence,
        };
        self.screens += 1;
        match partner {
            None => self.log_display(path, ScreenRole::Solo, None, &screen),
            Some(partner) => {
                self.log_display(path, ScreenRole::First, Some(partner), &screen);
                self.log_display(partner, ScreenRole::Second, Some(path), &screen);
            }
        }
    }

    fn log_display(
        &mut self,
        path: &Path,
        role: ScreenRole,
        partner: Option<&Path>,
        screen: &ScreenContext<'_>,
    ) {
        let playlist = screen.playlist;
        let pairable = screen.pairable;
        self.total += 1;
        let seq = self.total;
        let entry = self.history.entry(path.to_path_buf()).or_insert((0, 0));
        let first_show = entry.0 == 0;
        // Displays since this photo was last shown; -1 the first time it appears.
        // Expected ≈ inventory size under uniform random selection.
        let gap: i64 = if first_show {
            -1
        } else {
            (seq - entry.1) as i64
        };
        entry.0 += 1;
        entry.1 = seq;
        let shown_count = entry.0;
        let distinct = self.history.len();
        let inventory = playlist.inventory_len();
        let weight = playlist.current_weight(path).unwrap_or(0.0);
        let age_seconds = playlist.age_seconds(path).unwrap_or(0.0);
        let age_days = age_seconds / 86_400.0;
        let created_source = playlist
            .created_source(path)
            .map_or("unknown", CreatedSource::as_str);
        if screen.pairing {
            info!(
                seq,
                inventory,
                distinct,
                shown_count,
                gap,
                weight = format_args!("{weight:.2}"),
                age_days = format_args!("{age_days:.1}"),
                created_source = %created_source,
                screen = self.screens,
                pair = %role.as_str(),
                partner = ?partner.map(Path::display).map(|p| p.to_string()).unwrap_or_default(),
                pairable = pairable(path),
                sent = screen.evidence.sent.map_or(-1, |i| i as i64),
                removed = screen.evidence.removed,
                added = screen.evidence.added,
                path = %path.display(),
                "photo_display_metric"
            );
        } else {
            info!(
                seq,
                inventory,
                distinct,
                shown_count,
                gap,
                weight = format_args!("{weight:.2}"),
                age_days = format_args!("{age_days:.1}"),
                created_source = %created_source,
                path = %path.display(),
                "photo_display_metric"
            );
        }
        if first_show {
            // Precise birthtime → first-appearance latency, logged once per photo
            // (age_days is too coarse to see a minutes-scale staging lag). This is
            // a true staging latency only for photos added at runtime; for the
            // startup-scanned library it is just the photo's age, so filter on a
            // birthtime later than the `playlist_scheduler` line when analyzing.
            info!(
                seq,
                inventory,
                weight = format_args!("{weight:.2}"),
                latency_secs = format_args!("{age_seconds:.0}"),
                created_source = %created_source,
                path = %path.display(),
                "photo_first_shown"
            );
        }
    }
}

pub fn simulate_playlist<I>(
    photos: I,
    options: PlaylistOptions,
    now: SystemTime,
    iterations: usize,
    seed: Option<u64>,
) -> Vec<PathBuf>
where
    I: IntoIterator<Item = PhotoInfo>,
{
    let mut pl = build_scheduler(options, seed, Some(now));
    for info in photos {
        pl.record_add(info);
    }
    let mut plan = Vec::new();
    for _ in 0..iterations {
        match pl.peek_next() {
            Some((path, _priority)) => {
                plan.push((*path).clone());
                pl.commit_shown();
            }
            None => break,
        }
    }
    plan
}

/// The dry-run screen sequence: the scheduler's appearances grouped into
/// screens by the back-to-back planner (`planner`). `pairing` is `None` for
/// pairing off; `canvas` is the matting canvas pairability is judged on
/// (`None` = not yet published, so nothing pairs). Runs until at least
/// `iterations` appearances are planned. With pairing off every screen is
/// solo and the flattened sequence is `simulate_playlist`'s; with pairing on
/// it is too (P1), only grouped, unless `rules.lookahead > 1`, where every
/// appearance moves by at most `lookahead − 1` positions (P1-move).
pub fn simulate_screens<I>(
    photos: I,
    options: PlaylistOptions,
    pairing: Option<&PairingRules>,
    canvas: Option<(u32, u32)>,
    now: SystemTime,
    iterations: usize,
    seed: Option<u64>,
) -> Vec<PlannedScreen>
where
    I: IntoIterator<Item = PhotoInfo>,
{
    let mut pl = build_scheduler(options, seed, Some(now));
    let mut sizes = DisplaySizes::default();
    for info in photos {
        sizes.on_added(&info);
        pl.record_add(info);
    }
    let probability = pairing.map_or(0.0, |rules| rules.probability);
    let lookahead = pairing.map_or(1, |rules| rules.lookahead);
    let mut planner = ScreenPlanner::new(probability, seed).with_lookahead(lookahead);
    let is_pairable =
        |path: &Path| pairing.is_some_and(|rules| rules.is_pairable(sizes.get(path), canvas));
    let pairable: Option<&dyn Fn(&Path) -> bool> =
        (pairing.is_some() && canvas.is_some()).then_some(&is_pairable as _);
    let mut screens = Vec::new();
    let mut appearances = 0;
    while appearances < iterations {
        let Some(plan) = planner.plan(pl.as_mut(), pairable) else {
            break;
        };
        let screen = plan.screen();
        if let Plan::SendThenCommit(_) = plan {
            planner.commit_after_send(pl.as_mut());
        }
        planner.record_sent(&screen);
        appearances += screen.members().count();
        screens.push(screen);
    }
    screens
}

#[cfg(test)]
mod live_loop_tests {
    //! The live loop's screen handling (design test plan, "Live loop"), on a
    //! scripted scheduler whose log shows every peek and commit.
    use super::pairing::{LivePairing, PairingRules};
    use super::planner::scripted::{Log, Scripted};
    use super::*;
    use crate::events::{CanvasShape, MetaSource};

    fn info(name: &str, display_size: (u32, u32)) -> PhotoInfo {
        PhotoInfo {
            path: PathBuf::from(name),
            created_at: SystemTime::UNIX_EPOCH,
            created_source: CreatedSource::Birthtime,
            runtime_added: false,
            display_size: Some(display_size),
        }
    }

    /// `seq` played by a scripted scheduler; names starting with `L` are
    /// landscapes, the rest pairable portraits. `pairing` off when `None`.
    fn live(seq: &str, probability: Option<f64>) -> (LiveScreens, Log, CanvasShape) {
        live_with(seq, probability, 1)
    }

    /// [`live`] with a look-ahead window of `lookahead` positions.
    fn live_with(
        seq: &str,
        probability: Option<f64>,
        lookahead: usize,
    ) -> (LiveScreens, Log, CanvasShape) {
        let (sched, log) = Scripted::new(seq);
        let canvas = CanvasShape::default();
        canvas.publish(3840, 2160);
        let pairing = probability.map(|probability| LivePairing {
            rules: PairingRules {
                probability,
                bar_ratio: 0.618,
                max_upscale: 1.0,
                m_min: 0.02,
                m_max: 0.06,
                bevel_max: 0.0,
                lookahead,
            },
            canvas: canvas.clone(),
        });
        let mut screens = LiveScreens::new(Box::new(sched), pairing, Some(1));
        for name in seq.split_whitespace() {
            let size = if name.starts_with('L') {
                (4000, 3000)
            } else {
                (3000, 4000)
            };
            screens.sizes.on_added(&info(name, size));
        }
        (screens, log, canvas)
    }

    fn take(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    fn request(screens: &mut LiveScreens) -> String {
        let r = screens.next_request().expect("a screen");
        match r.partner {
            Some(partner) => format!("{}+{}", r.path.display(), partner.path.display()),
            None => r.path.display().to_string(),
        }
    }

    #[test]
    fn pairing_on_commits_p_and_x_before_the_send() {
        let (mut screens, log, _) = live("a b c d", Some(1.0));
        assert_eq!(request(&mut screens), "a+b");
        assert_eq!(
            take(&log),
            ["peek a", "commit a", "peek b", "commit b", "peek c"]
        );
        // A blocked send loops: the planned screen is fixed, nothing re-peeks.
        assert_eq!(request(&mut screens), "a+b");
        assert!(take(&log).is_empty());
        screens.sent();
        assert!(take(&log).is_empty(), "already committed");
        // The held peek (c) is the next screen's first member, not redrawn.
        assert_eq!(request(&mut screens), "c+d");
        assert_eq!(take(&log), ["commit c", "peek d", "commit d", "peek -"]);
    }

    /// Look-ahead in the live loop: the same `LoadPhoto` shapes (a pair, then
    /// solos), commits in sequence order, the shifted photos sent next without
    /// any scheduler call, and a removed queued photo dropped (B4).
    /// **Review C3, live timing.** (A, B) is planned at k = 3 with Q =
    /// [L1, A] and waits for a loader slot; L1 is removed during the wait;
    /// the pair is then sent. The next screen must not lead with A: C (the
    /// scheduler's front) goes first by today's path, then A.
    #[test]
    fn a_removal_while_the_pair_is_pending_does_not_repeat_its_head() {
        let (mut screens, log, _) = live_with("A L1 A B C", Some(1.0), 3);
        assert_eq!(request(&mut screens), "A+B");
        take(&log);
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("L1")));
        assert_eq!(request(&mut screens), "A+B", "the planned pair stays");
        screens.sent();
        assert_eq!(request(&mut screens), "C");
        assert_eq!(take(&log), ["remove L1", "peek C"]);
        // The blocked send re-plans identically.
        assert_eq!(request(&mut screens), "C");
        screens.sent();
        assert_eq!(take(&log), ["peek C", "commit C"]);
        assert_eq!(request(&mut screens), "A");
        screens.sent();
        assert_eq!(
            take(&log),
            ["peek -"],
            "A was already committed; its scan only finds the scheduler empty"
        );
        assert!(screens.next_request().is_none());
    }

    #[test]
    fn lookahead_sends_the_pair_then_the_shifted_queue_in_order() {
        let (mut screens, log, _) = live_with("a L1 L2 b c", Some(1.0), 3);
        assert_eq!(request(&mut screens), "a+b");
        assert_eq!(
            take(&log),
            [
                "peek a",
                "commit a",
                "peek L1",
                "commit L1",
                "peek L2",
                "commit L2",
                "peek b",
                "commit b",
                "peek c"
            ]
        );
        screens.sent();
        assert_eq!(request(&mut screens), "L1");
        assert!(take(&log).is_empty(), "already committed");
        screens.sent();
        // L2 is removed while queued: dropped, c (held) re-peeked.
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("L2")));
        assert_eq!(take(&log), ["remove L2"]);
        assert_eq!(request(&mut screens), "c");
        assert_eq!(
            take(&log),
            ["peek c", "commit c", "peek -"],
            "H was re-peeked (L2 never sent); c is pairable, so commit before send"
        );
        screens.sent();
        assert!(take(&log).is_empty());
        assert!(screens.next_request().is_none());
    }

    #[test]
    fn pairing_off_and_p0_keep_todays_peek_send_commit() {
        for probability in [None, Some(0.0)] {
            let (mut screens, log, _) = live("a b c", probability);
            assert_eq!(request(&mut screens), "a");
            assert_eq!(take(&log), ["peek a"]);
            // Each loop iteration re-peeks, as the loop always has.
            assert_eq!(request(&mut screens), "a");
            assert_eq!(take(&log), ["peek a"]);
            screens.sent();
            assert_eq!(take(&log), ["commit a"], "commit after the send");
            assert!(!screens.planner.holds_peek());
        }
    }

    #[test]
    fn b7_held_peek_survives_meta_and_canvas_but_not_add_or_remove() {
        // a pairs with nothing (L1 is a landscape): a goes alone, L1 is held.
        let (mut screens, log, canvas) = live("a L1 b c", Some(1.0));
        assert_eq!(request(&mut screens), "a");
        screens.sent();
        assert_eq!(take(&log), ["peek a", "commit a", "peek L1"]);
        assert!(screens.planner.holds_peek());

        screens.on_inventory(InventoryEvent::PhotoMeta {
            path: PathBuf::from("b"),
            display_size: (3000, 4000),
            source: MetaSource::Decoded,
        });
        canvas.publish(2560, 1600);
        assert!(
            screens.planner.holds_peek(),
            "PhotoMeta and CanvasShape keep H"
        );
        // L1 (held) is the next screen: today's path, no new peek.
        assert_eq!(request(&mut screens), "L1");
        assert!(take(&log).is_empty());

        let (mut screens, log, _) = live("a L1 b c", Some(1.0));
        request(&mut screens);
        screens.sent();
        take(&log);
        screens.on_inventory(InventoryEvent::PhotoAdded(info("new", (3000, 4000))));
        assert!(!screens.planner.holds_peek(), "PhotoAdded re-peeks");
        assert_eq!(request(&mut screens), "L1");
        assert_eq!(take(&log), ["add new", "peek L1"]);

        let (mut screens, log, _) = live("a L1 b c", Some(1.0));
        request(&mut screens);
        screens.sent();
        take(&log);
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("zzz")));
        assert!(!screens.planner.holds_peek(), "PhotoRemoved re-peeks");
    }

    #[test]
    fn b4_a_removed_member_degrades_the_planned_pair_and_an_empty_screen_is_replanned() {
        let (mut screens, log, _) = live("a b c d e", Some(1.0));
        assert_eq!(request(&mut screens), "a+b");
        // b is removed while the send is blocked: the pair degrades to a.
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("b")));
        assert_eq!(request(&mut screens), "a");
        // a too: the emptied screen is dropped and the next one planned.
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("a")));
        take(&log);
        assert_eq!(request(&mut screens), "c+d");
        assert_eq!(
            take(&log),
            ["peek c", "commit c", "peek d", "commit d", "peek e"]
        );
        // The degraded screen is the previous screen once sent (rule 4).
        let (mut screens, _, _) = live("a b c d", Some(1.0));
        request(&mut screens);
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("b")));
        screens.sent();
        assert_eq!(request(&mut screens), "c+d");
    }

    #[test]
    fn b4_a_removed_committed_first_member_is_dropped() {
        // A B A: rule 5 sends A alone and keeps B committed (C).
        let (mut screens, _, _) = live("A B A C D", Some(1.0));
        assert_eq!(request(&mut screens), "A");
        screens.sent();
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("B")));
        // C is gone; the next screen starts from a fresh peek.
        let next = request(&mut screens);
        assert!(!next.contains('B'), "{next}");
    }

    #[test]
    fn nothing_pairs_before_the_canvas_is_published() {
        let (sched, log) = Scripted::new("a b c");
        let pairing = LivePairing {
            rules: PairingRules {
                probability: 1.0,
                bar_ratio: 0.618,
                max_upscale: 1.0,
                m_min: 0.02,
                m_max: 0.06,
                bevel_max: 0.0,
                lookahead: 1,
            },
            canvas: CanvasShape::default(),
        };
        let mut screens = LiveScreens::new(Box::new(sched), Some(pairing), None);
        for name in ["a", "b", "c"] {
            screens.sizes.on_added(&info(name, (3000, 4000)));
        }
        assert_eq!(request(&mut screens), "a");
        assert_eq!(take(&log), ["peek a"], "today's path");
    }

    #[test]
    fn display_log_counts_appearances_and_screens() {
        let (screens, _, _) = live("a b", Some(1.0));
        let mut log = DisplayLog::default();
        let evidence = DisplayEvidence::default();
        log.log_screen(Path::new("a"), Some(Path::new("b")), evidence, &screens);
        log.log_screen(Path::new("c"), None, evidence, &screens);
        assert_eq!(log.total, 3, "one photo_display_metric line per appearance");
        assert_eq!(log.screens, 2);
    }

    #[test]
    fn displays_are_matched_to_their_send_with_removals_counted() {
        let (mut screens, _, _) = live("a b c d e f", Some(1.0));
        request(&mut screens);
        screens.sent(); // 1: a+b
        request(&mut screens);
        screens.sent(); // 2: c+d
        request(&mut screens);
        screens.sent(); // 3: e+f
        screens.on_inventory(InventoryEvent::PhotoRemoved(PathBuf::from("zzz")));
        screens.on_inventory(InventoryEvent::PhotoAdded(info("new", (3000, 4000))));
        let first = screens.on_displayed(Path::new("a"));
        assert_eq!(
            first,
            DisplayEvidence {
                sent: Some(1),
                removed: 1,
                added: 1
            }
        );
        // Screen 2 never displayed (purged): the next display jumps to 3.
        let next = screens.on_displayed(Path::new("e"));
        assert_eq!(
            next,
            DisplayEvidence {
                sent: Some(3),
                removed: 0,
                added: 0
            }
        );
        // A pair the loader degraded is led by its other member.
        assert_eq!(screens.on_displayed(Path::new("d")).sent, Some(2));
        assert_eq!(screens.on_displayed(Path::new("x")).sent, None);
    }

    /// The formatted `info!` output of `f`, as the journal would see it.
    fn capture(f: impl FnOnce()) -> String {
        use std::io::Write;
        use std::sync::Mutex;
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl Write for Buf {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
            type Writer = Buf;
            fn make_writer(&'a self) -> Buf {
                self.clone()
            }
        }
        let buf = Buf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .compact()
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buf.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    /// The `key=` names of the first line containing `message`, in order.
    fn field_names(log: &str, message: &str) -> Vec<String> {
        let line = log.lines().find(|l| l.contains(message)).expect(message);
        line.split_whitespace()
            .filter_map(|token| token.split_once('=').map(|(k, _)| k.to_string()))
            .collect()
    }

    /// Review F2: pairing off leaves `photo_display_metric` and the stamp
    /// exactly as before pairing.
    #[test]
    fn pairing_off_keeps_the_metric_lines_of_main() {
        let (mut screens, _, _) = live("a b", None);
        request(&mut screens);
        screens.sent();
        let evidence = screens.on_displayed(Path::new("a"));
        let mut log = DisplayLog::default();
        let out = capture(|| {
            log_scheduler_stamp(&PlaylistOptions::default(), None);
            log.log_screen(Path::new("a"), None, evidence, &screens);
        });
        assert_eq!(
            field_names(&out, "photo_display_metric"),
            [
                "seq",
                "inventory",
                "distinct",
                "shown_count",
                "gap",
                "weight",
                "age_days",
                "created_source",
                "path"
            ]
        );
        assert_eq!(
            field_names(&out, "playlist_scheduler"),
            [
                "order",
                "refractory",
                "min_spacing",
                "new_multiplicity",
                "half_life_secs"
            ]
        );
    }

    #[test]
    fn pairing_on_adds_the_screen_fields_and_prints_parameters_as_configured() {
        let (mut screens, _, _) = live("a b", Some(1.0));
        request(&mut screens);
        screens.sent();
        let evidence = screens.on_displayed(Path::new("a"));
        let mut log = DisplayLog::default();
        let out = capture(|| {
            log_scheduler_stamp(&PlaylistOptions::default(), Some(3));
            log.log_screen(Path::new("a"), Some(Path::new("b")), evidence, &screens);
        });
        let fields = field_names(&out, "photo_display_metric");
        for key in [
            "screen", "pair", "partner", "pairable", "sent", "removed", "added",
        ] {
            assert!(fields.iter().any(|f| f == key), "{key}: {fields:?}");
        }
        assert!(out.contains(" pair=first "), "{out}");
        assert!(out.contains(" pair=second "), "{out}");
        assert!(out.contains(" pairing_bar_ratio=0.618 "), "{out}");
        assert!(out.contains(" pairing_dwell_multiplier=1.5\n"), "{out}");
        assert!(out.contains(" pairing_probability=1 "), "{out}");
        assert!(out.contains(" pairing_lookahead=3 "), "{out}");
    }
}
