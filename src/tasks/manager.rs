use crate::events::{Displayed, InventoryEvent, LoadPhoto};
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Orchestrates the playlist and paces the show via the async send to `loader`.
///
/// Rules:
/// - Maintain a deduplicated `VecDeque<PathBuf>` playlist.
/// - On any `PhotoAdded`, push_front so new shots surface quickly.
/// - On `PhotoRemoved`, delete if present.
/// - Timing is paced by the async `.send()` to the loader.
/// - On successful send, rotate: pop_front -> push_back (keep cycling).
/// - Displayed notifications are informational; no re-queue on display.
pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    mut displayed_rx: Receiver<Displayed>,
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut playlist: VecDeque<PathBuf> = VecDeque::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    loop {
        // Prefer to make progress by sending when we have something.
        // Remain responsive to inventory + displayed events via `select!`.
        // Also include a small idle tick so startup (empty playlist) doesn't stall forever.
        select! {
            _ = cancel.cancelled() => break,

            // Drive slideshow by awaiting the send to the loader.
            // Rotate the playlist on successful send; viewer/loader handle pacing.
            res = {
                let next = playlist.front().cloned();
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
                        // Successfully queued: rotate front -> back to keep it in play.
                        if let Some(f) = playlist.pop_front() {
                            playlist.push_back(f);
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
                            // New to us: put it up front so it shows soon.
                            playlist.push_front(p);
                        } else {
                            // Already known; ignore.
                        }
                    }
                    Some(InventoryEvent::PhotoRemoved(p)) => {
                        if seen.remove(&p) {
                            // Remove from deque if present.
                            if let Some(pos) = playlist.iter().position(|q| q == &p) {
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
