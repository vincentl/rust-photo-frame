use crate::events::{InventoryEvent, InvalidPhoto, LoadPhoto};
use anyhow::Result;
use std::collections::VecDeque;
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
    let mut pending: VecDeque<PathBuf> = VecDeque::new();

    loop {
        // Opportunistic send to keep loader fed.
        if let Some(p) = pending.front().cloned() {
            if to_loader.try_send(LoadPhoto(p)).is_ok() {
                pending.pop_front();
                continue;
            }
        }

        select! {
            _ = cancel.cancelled() => break,

            Some(ev) = inv_rx.recv() => {
                match ev {
                    InventoryEvent::PhotoAdded(path) => {
                        // TODO: EXIF check; on failure:
                        // let _ = invalid_tx.send(InvalidPhoto(path)).await;
                        // else queue for load:
                        pending.push_back(path);
                    }
                    InventoryEvent::PhotoRemoved(path) => {
                        pending.retain(|p| p != &path);
                        // TODO: also remove from internal list/plan if you keep one.
                    }
                }
            }

            // If we have pending work but channel is full, await capacity alongside other events.
            _ = async {
                if let Some(p) = pending.front().cloned() {
                    let _ = to_loader.send(LoadPhoto(p)).await;
                }
            }, if !pending.is_empty() => {
                let _ = pending.pop_front();
            }
        }
    }
    Ok(())
}
