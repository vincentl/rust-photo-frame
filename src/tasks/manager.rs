use crate::events::{InvalidPhoto, InventoryEvent, LoadPhoto};
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    invalid_tx: Sender<InvalidPhoto>,
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    // Known photos + a short queue of pending loads.
    let mut known: HashSet<PathBuf> = HashSet::new();
    let mut pending: VecDeque<PathBuf> = VecDeque::new();

    // Avoid unused warning until EXIF validation is added.
    let _ = &invalid_tx;

    loop {
        // Opportunistic nonblocking send to keep the loader fed.
        if let Some(p) = pending.front().cloned() {
            if to_loader.try_send(LoadPhoto(p)).is_ok() {
                pending.pop_front();
                continue;
            }
        }

        select! {
            _ = cancel.cancelled() => break,

            Some(ev) = inv_rx.recv() => match ev {
                InventoryEvent::PhotoAdded(path) => {
                    // Insert returns true only if it was not present.
                    if known.insert(path.clone()) {
                        pending.push_back(path);
                    }
                }
                InventoryEvent::PhotoRemoved(path) => {
                    // Robust: ignore if we never saw it.
                    known.remove(&path);
                    pending.retain(|p| p != &path);
                }
            },

            // If queue is non-empty but channel lacked capacity, await a slot.
            _ = async {
                if let Some(p) = pending.front().cloned() {
                    let _ = to_loader.send(LoadPhoto(p)).await;
                }
            }, if !pending.is_empty() => {
                // Drop the item regardless of send result to avoid spins on closed channel.
                let _ = pending.pop_front();
            }
        }
    }
    Ok(())
}
