use crate::events::PhotoLoaded;
use anyhow::Result;
use tokio::select;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

pub async fn run(mut from_loader: Receiver<PhotoLoaded>, cancel: CancellationToken) -> Result<()> {
    loop {
        select! {
            _ = cancel.cancelled() => break,
            Some(PhotoLoaded(photo)) = from_loader.recv() => {
                println!("displaying: {}", photo.path.display());
                // TODO: timer + crossfade
            }
        }
    }
    Ok(())
}
