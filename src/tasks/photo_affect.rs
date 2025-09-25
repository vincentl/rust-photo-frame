use anyhow::{Context, Result};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::config::{PhotoAffectConfig, PhotoAffectKind, PhotoAffectMode};
use crate::events::PhotoLoaded;
use crate::processing::affect::apply_print_relief;

pub async fn run(
    mut from_loader: Receiver<PhotoLoaded>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
    config: PhotoAffectConfig,
) -> Result<()> {
    let enabled = config.is_enabled();
    loop {
        select! {
            _ = cancel.cancelled() => {
                debug!("photo-affect task cancelled");
                break;
            },
            maybe_msg = from_loader.recv() => {
                match maybe_msg {
                    Some(PhotoLoaded(mut prepared)) => {
                        if enabled {
                            if let Some(kind) = config.next_kind() {
                                if let Some(mode) = config.options_for(kind) {
                                    if let Err(err) = apply_affect(kind, mode, &mut prepared) {
                                        warn!(
                                            path = %prepared.path.display(),
                                            ?err,
                                            "photo affect failed; forwarding original image"
                                        );
                                    } else {
                                        trace!(path = %prepared.path.display(), ?kind, "applied photo affect");
                                    }
                                } else {
                                    warn!(?kind, "missing configuration for selected photo affect");
                                }
                            }
                        }
                        if to_viewer.send(PhotoLoaded(prepared)).await.is_err() {
                            debug!("viewer channel closed");
                            break;
                        }
                    }
                    None => {
                        trace!("loader channel closed; stopping photo-affect task");
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn apply_affect(
    kind: PhotoAffectKind,
    mode: &PhotoAffectMode,
    prepared: &mut crate::events::PreparedImageCpu,
) -> Result<()> {
    let result: Result<()> = match mode {
        PhotoAffectMode::Print3d(options) => {
            let Some(mut image) = image::RgbaImage::from_raw(
                prepared.width,
                prepared.height,
                std::mem::take(&mut prepared.pixels),
            ) else {
                anyhow::bail!("invalid image buffer for print-3d affect");
            };
            apply_print_relief(&mut image, options);
            prepared.pixels = image.into_raw();
            Ok(())
        }
    };

    result.with_context(|| format!("failed to apply {:?} photo affect", kind))
}
