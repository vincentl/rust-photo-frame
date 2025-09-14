use crate::events::{InvalidPhoto, InventoryEvent, LoadPhoto};
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::{debug, info, warn};

#[instrument(skip(inv_rx, _invalid_tx, to_loader, cancel), name = "manager")]
pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    _invalid_tx: Sender<InvalidPhoto>, // reserved for future EXIF/validation
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut known: HashSet<PathBuf> = HashSet::new();
    let mut pending: VecDeque<PathBuf> = VecDeque::new();

    info!("started");

    loop {
        // Opportunistic non-blocking send.
        if let Some(p) = pending.front().cloned() {
            if to_loader.try_send(LoadPhoto(p.clone())).is_ok() {
                debug!("-> loader (try_send): {}", p.display());
                pending.pop_front();
                continue;
            }
        }

        select! {
            _ = cancel.cancelled() => {
                info!("cancel received; exiting");
                break;
            }

            Some(ev) = inv_rx.recv() => {
                match ev {
                    InventoryEvent::PhotoAdded(path) => {
                        if known.insert(path.clone()) {
                            info!("added: {}", path.display());
                            pending.push_back(path);
                        } else {
                            debug!("duplicate add (ignored): {}", path.display());
                        }
                        debug!(known = known.len(), pending = pending.len(), "state");
                    }
                    InventoryEvent::PhotoRemoved(path) => {
                        let was_known = known.remove(&path);
                        let before = pending.len();
                        pending.retain(|p| p != &path);
                        let removed_pending = before != pending.len();

                        if was_known || removed_pending {
                            info!(
                                "removed: {} (known_removed={}, pending_removed={})",
                                path.display(), was_known, removed_pending
                            );
                        } else {
                            warn!("spurious remove (ignored): {}", path.display());
                        }
                        debug!(known = known.len(), pending = pending.len(), "state");
                    }
                }
            }

            _ = async {
                if let Some(p) = pending.front().cloned() {
                    match to_loader.send(LoadPhoto(p.clone())).await {
                        Ok(()) => debug!("-> loader (await): {}", p.display()),
                        Err(e) => warn!("send to loader failed for {}: {e}", p.display()),
                    }
                }
            }, if !pending.is_empty() => {
                let _ = pending.pop_front();
            }
        }
    }

    Ok(())
}
