use crate::events::{InvalidPhoto, LoadPhoto, PhotoLoaded, PreparedImageCpu};
use anyhow::Result;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

// Decodes an image to RGBA8 and applies EXIF orientation if available.
// Note: This uses the `image` crate. Orientation handling is a best-effort; if
// metadata is missing, the original orientation is preserved.
fn decode_rgba8_apply_exif(path: &Path) -> anyhow::Result<image::RgbaImage> {
    // Read and decode
    let img = image::ImageReader::open(path)?
        .with_guessed_format()? // sniff based on content/extension
        .decode()?; // DynamicImage

    // Convert to RGBA8 early so that subsequent ops work on a concrete buffer
    let mut img = img.to_rgba8();

    // Attempt EXIF orientation correction
    let orientation: u16 = read_orientation(path).unwrap_or(1);
    // Map common EXIF orientations. Unsupported cases fall through as-is.
    match orientation {
        1 => {}
        2 => {
            // horizontal flip
            img = image::imageops::flip_horizontal(&img);
        }
        3 => {
            img = image::imageops::rotate180(&img);
        }
        4 => {
            // vertical flip
            img = image::imageops::flip_vertical(&img);
        }
        5 => {
            // transpose (flip diag): rotate90 + flip_horizontal
            img = image::imageops::rotate90(&img);
            img = image::imageops::flip_horizontal(&img);
        }
        6 => {
            // rotate 90 CW
            img = image::imageops::rotate90(&img);
        }
        7 => {
            // transverse: rotate270 + flip_horizontal
            img = image::imageops::rotate270(&img);
            img = image::imageops::flip_horizontal(&img);
        }
        8 => {
            // rotate 270 CW
            img = image::imageops::rotate270(&img);
        }
        _ => {}
    }

    Ok(img)
}

fn read_orientation(path: &Path) -> Option<u16> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    if let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
        if let Some(val) = field.value.get_uint(0) {
            let o = val as u16;
            debug!("exif orientation {} for {}", o, path.display());
            return Some(o);
        }
    }
    None
}

/// Very simple loader:
/// - Reads the bytes (to prove existence) and forwards a `PreparedPhoto`.
/// - On I/O error, emits `InvalidPhoto`.
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
                match decode_rgba8_apply_exif(&path) {
                    Ok(rgba8) => {
                        debug!("loaded (rgba8): {}", path.display());
                        let (width, height) = rgba8.dimensions();
                        let prepared = PreparedImageCpu {
                            path: path.clone(),
                            width,
                            height,
                            pixels: rgba8.into_raw(),
                        };
                        let _ = to_viewer.send(PhotoLoaded(prepared)).await;
                    }
                    Err(e) => {
                        debug!("invalid photo {}: {}", path.display(), e);
                        let _ = invalid_tx.send(InvalidPhoto(path)).await;
                    }
                }
            }
            else => break,
        }
    }
    Ok(())
}
