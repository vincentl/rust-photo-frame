use crate::events::{Displayed, InventoryEvent, LoadPhoto};
use anyhow::Result;
use rand::distributions::{Distribution, WeightedIndex};
use rand::SeedableRng;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const BASE_WEIGHT: f32 = 1.0;
const BOOST_EXTRA: f32 = 3.0;
const WEIGHT_DECAY: f32 = 0.6;
const REPEAT_SUPPRESSION_RATIO: f32 = 1.25;

#[derive(Debug, Clone)]
struct WeightedPhoto {
    path: PathBuf,
    extra_weight: f32,
}

impl WeightedPhoto {
    fn boosted(path: PathBuf) -> Self {
        Self {
            path,
            extra_weight: BOOST_EXTRA,
        }
    }

    fn decay(&mut self) {
        self.extra_weight = (self.extra_weight * WEIGHT_DECAY).max(0.0);
    }

    fn weight(&self) -> f32 {
        BASE_WEIGHT + self.extra_weight.max(0.0)
    }
}

fn select_weighted_index(
    playlist: &[WeightedPhoto],
    last_displayed: Option<&PathBuf>,
    rng: &mut rand::rngs::StdRng,
) -> Option<usize> {
    if playlist.is_empty() {
        return None;
    }
    if playlist.len() == 1 {
        return Some(0);
    }

    let weights: Vec<f32> = playlist.iter().map(|p| p.weight()).collect();
    let dist = WeightedIndex::new(weights).ok()?;
    let choice = dist.sample(rng);

    if let Some(last) = last_displayed {
        if playlist.len() > 1 && playlist[choice].path == *last {
            let chosen_weight = playlist[choice].weight();
            let mut max_other: f32 = 0.0;
            for photo in playlist.iter() {
                if &photo.path != last {
                    max_other = max_other.max(photo.weight());
                }
            }
            if max_other > 0.0 && chosen_weight < max_other * REPEAT_SUPPRESSION_RATIO {
                let mut filtered_weights = Vec::with_capacity(playlist.len() - 1);
                let mut filtered_indices = Vec::with_capacity(playlist.len() - 1);
                for (idx, photo) in playlist.iter().enumerate() {
                    if &photo.path != last {
                        filtered_indices.push(idx);
                        filtered_weights.push(photo.weight());
                    }
                }
                if filtered_indices.is_empty() {
                    return Some(choice);
                }
                if let Ok(filtered) = WeightedIndex::new(filtered_weights) {
                    let idx_in_filtered = filtered.sample(rng);
                    return Some(filtered_indices[idx_in_filtered]);
                } else {
                    return filtered_indices.first().copied();
                }
            }
        }
    }

    Some(choice)
}

/// Orchestrates the playlist and paces the show via the async send to `loader`.
///
/// Rules:
/// - Maintain a deduplicated weighted playlist.
/// - Newly added photos receive a higher weight so they surface more often.
/// - After a photo is queued for display its weight decays toward the baseline.
/// - Selection prefers higher weights but avoids showing the same photo twice
///   consecutively when alternatives exist.
pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    mut displayed_rx: Receiver<Displayed>,
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut playlist: Vec<WeightedPhoto> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut last_displayed: Option<PathBuf> = None;
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xDEC0_D1A5);

    loop {
        // Prefer to make progress by sending when we have something.
        // Remain responsive to inventory + displayed events via `select!`.
        // Also include a small idle tick so startup (empty playlist) doesn't stall forever.
        let mut selected_index: Option<usize> = None;
        select! {
            _ = cancel.cancelled() => break,

            // Drive slideshow by awaiting the send to the loader.
            // Weighting is handled outside the future so the send path stays async-friendly.
            res = {
                let next = if playlist.is_empty() {
                    None
                } else {
                    selected_index = select_weighted_index(&playlist, last_displayed.as_ref(), &mut rng);
                    selected_index
                        .and_then(|idx| playlist.get(idx))
                        .map(|entry| entry.path.clone())
                };
                let to_loader = to_loader.clone();
                async move {
                    if let Some(p) = next {
                        to_loader.send(LoadPhoto(p)).await.map(|_| ()).map_err(|_| ())
                    } else {
                        Err(())
                    }
                }
            }, if !playlist.is_empty() => {
                match res {
                    Ok(()) => {
                        if let Some(idx) = selected_index {
                            if let Some(entry) = playlist.get_mut(idx) {
                                let shown = entry.path.clone();
                                entry.decay();
                                last_displayed = Some(shown);
                            }
                        }
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
                    Some(InventoryEvent::PhotoAdded(p)) => {
                        if seen.insert(p.clone()) {
                            for entry in &mut playlist {
                                entry.extra_weight = (entry.extra_weight * WEIGHT_DECAY).max(0.0);
                            }
                            playlist.push(WeightedPhoto::boosted(p));
                        } else {
                            // Already known; ignore.
                        }
                    }
                    Some(InventoryEvent::PhotoRemoved(p)) => {
                        if seen.remove(&p) {
                            // Remove from deque if present.
                            if let Some(pos) = playlist.iter().position(|q| q.path == p) {
                                playlist.remove(pos);
                            }
                        }
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
                    last_displayed = Some(p);
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
