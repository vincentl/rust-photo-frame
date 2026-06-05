use crate::config::PlaylistOptions;
use crate::events::{Displayed, InventoryEvent, LoadPhoto, PhotoInfo};
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
use tracing::{debug, warn};

/// Orchestrates the playlist via a virtual-time min-heap scheduler.
///
/// Rules:
/// - Each photo has a scheduling key drawn from an exponential gap distribution
///   inversely proportional to its weight. Higher weight ⇒ smaller mean gap ⇒ shown more often.
/// - The photo with the smallest key is always shown next.
/// - On show, the photo is rescheduled at vclock + new gap (no rebuild needed).
/// - `PhotoAdded` / `PhotoRemoved` are O(log n) heap ops; removed entries are lazily skipped.
pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    mut displayed_rx: Receiver<Displayed>,
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
    options: PlaylistOptions,
    now_override: Option<SystemTime>,
    seed_override: Option<u64>,
) -> Result<()> {
    let rng = match seed_override {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_os_rng(),
    };
    let mut playlist = PlaylistState::with_rng(options, rng, now_override);

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
                    debug!("displayed: {}", p.display());
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
    now_override: Option<SystemTime>,
}

struct Meta {
    created_at: SystemTime,
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
        Self {
            heap: BinaryHeap::new(),
            known: HashMap::new(),
            generations: HashMap::new(),
            vclock: 0.0,
            seq: 0,
            rng,
            options,
            now_override,
        }
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    /// Exponential gap with mean 1/weight (Poisson scheduling). u in (0,1] avoids ln(0).
    fn sample_gap(&mut self, weight: f64) -> f64 {
        let u = 1.0 - self.rng.random::<f64>(); // random::<f64>() ∈ [0,1), so u ∈ (0,1]
        -u.ln() / weight.max(1.0)
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
        self.heap.push(Entry { key, seq, generation, path });
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

    fn record_add(&mut self, info: PhotoInfo) {
        // Already live (e.g. a metadata refresh): update created_at but keep the existing
        // schedule and generation — do not push another heap entry.
        if let Some(meta) = self.known.get_mut(&info.path) {
            meta.created_at = info.created_at;
            return;
        }
        // New, or re-added after removal. Reading the bumped generation here ensures the
        // fresh heap entry has a strictly higher generation than any orphaned stale entries.
        let created_at = info.created_at;
        let path_arc = Arc::new(info.path);
        let generation = *self.generations.entry((*path_arc).clone()).or_insert(0);
        let weight = self.options.weight_for(created_at, self.now());
        self.known.insert(
            (*path_arc).clone(),
            Meta { created_at, generation, shown: false },
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

    /// Pop the earliest still-valid entry, advance vclock, mark shown, and reschedule.
    /// Used by `simulate_playlist` where peek+commit can be a single call.
    fn pop_next(&mut self) -> Option<(Arc<PathBuf>, bool)> {
        while let Some(entry) = self.heap.pop() {
            let valid = self
                .known
                .get(entry.path.as_ref())
                .is_some_and(|m| m.generation == entry.generation);
            if !valid {
                continue;
            }
            self.vclock = entry.key;
            let path = entry.path.clone();
            let (created_at, priority) = {
                let meta = self.known.get_mut(entry.path.as_ref()).expect("validated above");
                let p = !meta.shown;
                meta.shown = true;
                (meta.created_at, p)
            };
            self.reschedule_after_show(Arc::clone(&path), created_at, entry.generation);
            return Some((path, priority));
        }
        None
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
    let rng = match seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_os_rng(),
    };
    let mut pl = PlaylistState::with_rng(options, rng, Some(now));
    for info in photos {
        pl.record_add(info);
    }
    let mut plan = Vec::new();
    for _ in 0..iterations {
        match pl.pop_next() {
            Some((path, _priority)) => plan.push((*path).clone()),
            None => break,
        }
    }
    plan
}
