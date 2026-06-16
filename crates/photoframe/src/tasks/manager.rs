use crate::config::PlaylistOptions;
use crate::events::{CreatedSource, Displayed, InventoryEvent, LoadPhoto, PhotoInfo};
use anyhow::Result;
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

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
) -> Result<()> {
    if metrics {
        // Stamp the scheme + parameters once per process so a grepped
        // photo_display_metric series can be interpreted (and segmented across
        // sessions / config changes) without guessing what produced it.
        info!(
            order = %options.order,
            refractory = format_args!("{:.3}", options.refractory_fraction()),
            min_spacing = format_args!("{:.3}", options.min_spacing),
            new_multiplicity = options.new_multiplicity,
            half_life_secs = options.half_life.as_secs(),
            "playlist_scheduler"
        );
    }
    let mut playlist = build_scheduler(options, seed_override, now_override);
    let mut display_log = DisplayLog::default();

    loop {
        let next = playlist.peek_next();
        let next_is_some = next.is_some();

        select! {
            _ = cancel.cancelled() => break,

            // Drive slideshow by sending the next photo to the loader.
            // Commit (pop + reschedule) only after a successful send so no slide is lost.
            res = {
                let to_loader = to_loader.clone();
                async move {
                    match next {
                        Some((path, priority)) => to_loader
                            .send(LoadPhoto { path: (*path).clone(), priority })
                            .await
                            .map_err(|_| ()),
                        None => Err(()),
                    }
                }
            }, if next_is_some => {
                match res {
                    Ok(()) => playlist.commit_shown(),
                    Err(()) => {
                        warn!("loader channel closed");
                        break;
                    }
                }
            }

            // Inventory updates (from files task)
            maybe_ev = inv_rx.recv() => match maybe_ev {
                Some(InventoryEvent::PhotoAdded(info)) => playlist.record_add(info),
                Some(InventoryEvent::PhotoRemoved(p)) => playlist.record_remove(&p),
                None => {}
            },

            // Displayed notifications (informational only)
            maybe_disp = displayed_rx.recv() => {
                if let Some(Displayed(p)) = maybe_disp {
                    if metrics {
                        display_log.log_display(&p, playlist.as_ref());
                    } else {
                        debug!("displayed: {}", p.display());
                    }
                }
            }

            // Idle tick: prevents spinning when the heap is empty at startup.
            _ = sleep(Duration::from_millis(50)) => {}
        }
    }

    Ok(())
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
        self.schedule(path_arc, created_at, generation);
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

    /// Drain leading tombstoned/stale entries off the heap, then return the front entry's
    /// path and priority (`!shown`) without popping or marking it shown. Returns `None` when
    /// the heap is empty or all entries are invalid.
    fn peek_next(&mut self) -> Option<(Arc<PathBuf>, bool)> {
        loop {
            let (path, generation) = match self.heap.peek() {
                None => return None,
                Some(entry) => (entry.path.clone(), entry.generation),
            };
            let valid = self
                .known
                .get(path.as_ref())
                .is_some_and(|m| m.generation == generation);
            if valid {
                let priority = !self.known[path.as_ref()].shown;
                return Some((path, priority));
            }
            self.heap.pop(); // tombstone / stale → drop
        }
    }

    /// Pop the front entry (the one `peek_next` just returned), advance vclock, mark it
    /// shown, and reschedule it. Defensively re-validates before committing.
    fn commit_shown(&mut self) {
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
    /// Per-path running history: (times shown, `seq` of the most recent show).
    history: HashMap<PathBuf, (u64, u64)>,
}

impl DisplayLog {
    fn log_display(&mut self, path: &Path, playlist: &dyn PlaylistScheduler) {
        self.total += 1;
        let seq = self.total;
        let entry = self.history.entry(path.to_path_buf()).or_insert((0, 0));
        // Displays since this photo was last shown; -1 the first time it appears.
        // Expected ≈ inventory size under uniform random selection.
        let gap: i64 = if entry.0 == 0 {
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
        let age_days = playlist.age_seconds(path).unwrap_or(0.0) / 86_400.0;
        let created_source = playlist
            .created_source(path)
            .map_or("unknown", CreatedSource::as_str);
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
