use crate::events::{Displayed, InventoryEvent, LoadPhoto};
use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::{debug, info, warn};

#[instrument(skip(inv_rx, displayed_rx, to_loader, cancel), name = "manager")]
pub async fn run(
    mut inv_rx: Receiver<InventoryEvent>,
    mut displayed_rx: Receiver<Displayed>,
    to_loader: Sender<LoadPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    // Catalog = membership; Playlist = order (front is next to send).
    let mut catalog: HashSet<PathBuf> = HashSet::new();
    let mut playlist: VecDeque<PathBuf> = VecDeque::new();

    info!("started");

    loop {
        // Opportunistic non-blocking send of the current front.
        if let Some(p) = playlist.front().cloned() {
            if to_loader.try_send(LoadPhoto(p.clone())).is_ok() {
                debug!("-> loader (try_send): {}", p.display());
                // rotate on success
                let f = playlist.pop_front().unwrap();
                playlist.push_back(f);
                continue;
            }
        }

        select! {
            _ = cancel.cancelled() => {
                info!("cancel received; exiting");
                break;
            }

            Some(ev) = inv_rx.recv() => match ev {
                InventoryEvent::PhotoAdded(path) => {
                    if catalog.insert(path.clone()) {
                        // New photos go to the front so they appear quickly.
                        playlist.push_front(path.clone());
                        info!("added: {}", path.display());
                    } else {
                        debug!("duplicate add (ignored): {}", path.display());
                    }
                    let front_disp = playlist.front().map(|p| p.display().to_string()).unwrap_or_else(|| "<none>".to_string());
                    debug!(catalog = catalog.len(), playlist = playlist.len(), front = %front_disp, "state");
                }
                InventoryEvent::PhotoRemoved(path) => {
                    if catalog.remove(&path) {
                        // Remove first/only occurrence from playlist
                        if let Some(pos) = playlist.iter().position(|p| p == &path) {
                            let removed_front = pos == 0;
                            playlist.remove(pos);
                            info!("removed: {} (was_front={})", path.display(), removed_front);
                        } else {
                            info!("removed: {} (not in playlist—already rotated)", path.display());
                        }
                    } else {
                        warn!("spurious remove (ignored): {}", path.display());
                    }
                    let front_disp = playlist.front().map(|p| p.display().to_string()).unwrap_or_else(|| "<none>".to_string());
                    debug!(catalog = catalog.len(), playlist = playlist.len(), front = %front_disp, "state");
                }
            },

            // Viewer notification (informational for now)
            Some(Displayed(path)) = displayed_rx.recv() => {
                debug!("displayed: {}", path.display());
            }

            // Await capacity if channel was full; rotate only on success.
            res = to_loader.reserve(), if !playlist.is_empty() => {
                match res {
                    Ok(permit) => {
                        if let Some(p) = playlist.front().cloned() {
                            permit.send(LoadPhoto(p.clone()));
                            debug!("-> loader (await): {}", p.display());
                            let f = playlist.pop_front().unwrap();
                            playlist.push_back(f);
                        }
                    }
                    Err(_) => {
                        warn!("loader channel closed");
                    }
                }
            }
        }
    }

    Ok(())
}
