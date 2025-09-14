use crate::events::{Displayed, PhotoLoaded};
use anyhow::Result;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub async fn run(
    mut from_loader: Receiver<PhotoLoaded>,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        select! {
            _ = cancel.cancelled() => break,
            Some(PhotoLoaded(photo)) = from_loader.recv() => {
                debug!("displaying: {}", photo.path.display());
                // TODO: real timer + crossfade; for now, immediately report displayed
                let _ = to_manager_displayed.send(Displayed(photo.path)).await;
            }
        }
    }
    Ok(())
}
