use crate::events::{InvalidPhoto, LoadPhoto, PhotoLoaded, PreparedPhoto};
use anyhow::Result;
use std::fs;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

pub async fn run(
    mut load_rx: Receiver<LoadPhoto>,
    invalid_tx: Sender<InvalidPhoto>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        select! {
            _ = cancel.cancelled() => break,
            Some(LoadPhoto(path)) = load_rx.recv() => {
                match fs::read(&path) {
                    Ok(_bytes) => {
                        // TODO: decode, orient, prepare GPU-friendly buffer.
                        let photo = PreparedPhoto { path };
                        let _ = to_viewer.send(PhotoLoaded(photo)).await;
                    }
                    Err(_) => {
                        let _ = invalid_tx.send(InvalidPhoto(path)).await;
                    }
                }
            }
        }
    }
    Ok(())
}
