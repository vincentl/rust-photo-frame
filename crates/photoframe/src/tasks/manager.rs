use crate::config::PlaylistOptions;
use crate::events::{Displayed, InventoryEvent, LoadPhoto, PhotoInfo};
use anyhow::Result;
use rand::{SeedableRng, rngs::StdRng, seq::SliceRandom};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Orchestrates the playlist and paces the show via the async send to `loader`.
///
/// Rules:
/// - Maintain a weighted `VecDeque<PathBuf>` playlist, duplicating photos by multiplicity.
/// - On `PhotoAdded`, record metadata and prioritize the new image at the front of the next cycle.
/// - On `PhotoRemoved`, drop all scheduled occurrences and forget future weighting.
/// - Timing is paced by the async `.send()` to the loader.
/// - On successful send, consume that scheduled occurrence; rebuild the queue when exhausted or dirty.
/// - Displayed notifications are informational; no re-queue on display.
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
        playlist.ensure_ready();

        // Prefer to make progress by sending when we have something.
        // Remain responsive to inventory + displayed events via `select!`.
        // Also include a small idle tick so startup (empty playlist) doesn't stall forever.
        select! {
            _ = cancel.cancelled() => break,

            // Drive slideshow by awaiting the send to the loader.
            // Rotate the playlist on successful send; viewer/loader handle pacing.
            res = {
                let next = playlist.peek().cloned();
                let to_loader = to_loader.clone();
                async move {
                    if let Some(p) = next {
                        let load = LoadPhoto {
                            path: (*p.path).clone(),
                            priority: p.priority,
                        };
                        to_loader.send(load).await.map(|_| p).map_err(|_| ())
                    } else {
                        Err(())
                    }
                }
            }, if !playlist.is_empty() => {
                match res {
                    Ok(sent) => {
                        playlist.mark_sent(&sent);
                    }
                    Err(_) => {
                        warn!("loader channel closed");
                        // Break rather than spin forever with a dead peer.
                        break;
                    }
                }
            }

            // Inventory updates (from files task)
            maybe_ev = inv_rx.recv() => {
                match maybe_ev {
                    Some(InventoryEvent::PhotoAdded(info)) => {
                        playlist.record_add(info);
                    }
                    Some(InventoryEvent::PhotoRemoved(p)) => {
                        playlist.record_remove(&p);
                    }
                    None => {
                        // Inventory producer ended. We can continue with what we have.
                        // No action.
                    }
                }
            }

            // Displayed notifications (from viewer)
            maybe_disp = displayed_rx.recv() => {
                if let Some(Displayed(p)) = maybe_disp {
                    debug!("displayed: {}", p.display());
                    // No action required; we keep the playlist rotating regardless.
                } else {
                    // Viewer side closed; nothing fatal.
                }
            }

            // Idle tick: if nothing else is happening (e.g., startup with empty playlist),
            // wake up periodically to re-evaluate conditions.
            _ = sleep(Duration::from_millis(50)) => {
                // No-op; the loop will iterate and try again.
            }
        }
    }

    Ok(())
}

struct PlaylistState {
    queue: VecDeque<ScheduledPhoto>,
    known: HashMap<PathBuf, PhotoInfo>,
    prioritized: Vec<PathBuf>,
    rng: StdRng,
    options: PlaylistOptions,
    dirty: bool,
    now_override: Option<SystemTime>,
}

#[derive(Clone)]
struct ScheduledPhoto {
    path: Arc<PathBuf>,
    priority: bool,
}

impl PlaylistState {
    fn with_rng(options: PlaylistOptions, rng: StdRng, now_override: Option<SystemTime>) -> Self {
        Self {
            queue: VecDeque::new(),
            known: HashMap::new(),
            prioritized: Vec::new(),
            rng,
            options,
            dirty: true,
            now_override,
        }
    }

    fn ensure_ready(&mut self) {
        if self.dirty {
            self.rebuild(RebuildReason::InventoryChange);
        } else if self.queue.is_empty() {
            // When the queue drains naturally we rebuild to reshuffle the next lap.
            // This used to log at info! every cycle, which was noisy despite being expected.
            self.rebuild(RebuildReason::CycleExhausted);
        }
    }

    fn rebuild(&mut self, reason: RebuildReason) {
        if self.known.is_empty() {
            self.queue.clear();
            self.dirty = false;
            self.prioritized.clear();
            return;
        }

        let now = self.now_override.unwrap_or_else(SystemTime::now);
        let prioritized = std::mem::take(&mut self.prioritized);
        let mut front: Vec<ScheduledPhoto> = Vec::new();
        let mut rest: Vec<ScheduledPhoto> = Vec::new();
        let mut multiplicities: Vec<(&Path, usize)> = Vec::new();
        let prioritized_len;

        let mut infos: Vec<&PhotoInfo> = self.known.values().collect();
        infos.sort_by(|a, b| a.path.cmp(&b.path));

        {
            let prioritized_set: HashSet<&Path> =
                prioritized.iter().map(PathBuf::as_path).collect();
            prioritized_len = prioritized_set.len();

            for info in infos {
                let multiplicity = self.options.multiplicity_for(info.created_at, now);
                if multiplicity == 0 {
                    continue;
                }
                let info_path = info.path.as_path();
                multiplicities.push((info_path, multiplicity));
                let shared_path = Arc::new(info.path.clone());
                if prioritized_set.contains(info_path) {
                    front.push(ScheduledPhoto {
                        path: Arc::clone(&shared_path),
                        priority: true,
                    });
                    for _ in 1..multiplicity {
                        rest.push(ScheduledPhoto {
                            path: Arc::clone(&shared_path),
                            priority: false,
                        });
                    }
                } else {
                    for _ in 0..multiplicity {
                        rest.push(ScheduledPhoto {
                            path: Arc::clone(&shared_path),
                            priority: false,
                        });
                    }
                }
            }
        }

        rest.shuffle(&mut self.rng);

        let mut queue = VecDeque::new();
        for path in prioritized {
            if let Some(idx) = front
                .iter()
                .position(|p| p.path.as_path() == path.as_path())
            {
                queue.push_back(front.remove(idx));
            }
        }
        for entry in front {
            queue.push_back(entry);
        }
        for entry in rest {
            queue.push_back(entry);
        }

        self.queue = queue;
        self.dirty = false;

        for (path, multiplicity) in &multiplicities {
            debug!(
                path = %path.display(),
                multiplicity,
                now = ?now,
                "playlist multiplicity"
            );
        }
        match reason {
            RebuildReason::InventoryChange => info!(
                photos = multiplicities.len(),
                scheduled = self.queue.len(),
                prioritized = prioritized_len,
                now = ?now,
                reason = ?reason,
                "playlist rebuilt"
            ),
            RebuildReason::CycleExhausted => debug!(
                photos = multiplicities.len(),
                scheduled = self.queue.len(),
                prioritized = prioritized_len,
                now = ?now,
                reason = ?reason,
                "playlist rebuilt"
            ),
        }
    }

    fn peek(&self) -> Option<&ScheduledPhoto> {
        self.queue.front()
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn mark_sent(&mut self, sent: &ScheduledPhoto) {
        if let Some(front) = self.queue.front() {
            if front.path == sent.path {
                self.queue.pop_front();
                return;
            }
        }
        if let Some(pos) = self.queue.iter().position(|p| p.path == sent.path) {
            self.queue.remove(pos);
        }
    }

    fn record_add(&mut self, info: PhotoInfo) {
        let path = info.path.clone();
        let was_new = self.known.insert(info.path.clone(), info).is_none();
        if was_new && !self.prioritized.iter().any(|p| p == &path) {
            self.prioritized.push(path);
        }
        self.dirty = true;
    }

    fn record_remove(&mut self, path: &Path) {
        if self.known.remove(path).is_some() {
            self.prioritized.retain(|p| p != path);
            self.queue.retain(|p| p.path.as_path() != path);
            self.dirty = true;
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RebuildReason {
    InventoryChange,
    CycleExhausted,
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
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_os_rng(),
    };
    let mut playlist = PlaylistState::with_rng(options, rng, Some(now));

    for info in photos {
        playlist.record_add(info);
    }

    let mut plan = Vec::new();
    for _ in 0..iterations {
        playlist.ensure_ready();
        if let Some(next) = playlist.peek().cloned() {
            plan.push((*next.path).clone());
            playlist.mark_sent(&next);
        } else {
            break;
        }
    }

    plan
}
