use crate::events::{InvalidPhoto, LoadPhoto, PhotoLoaded, PreparedImageCpu};
use anyhow::Result;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::JoinSet;
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
    max_in_flight: usize,
) -> Result<()> {
    let mut in_flight: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let mut tasks: JoinSet<(std::path::PathBuf, Option<image::RgbaImage>)> = JoinSet::new();
    let mut pending_ready: Option<ReadyPhoto> = None;
    let mut last_sent_path: Option<PathBuf> = None;

    loop {
        select! {
            _ = cancel.cancelled() => {
                flush_pending(&mut last_sent_path, &mut pending_ready, &to_viewer).await;
                break;
            },

            // Accept new load requests while under limit
            Some(LoadPhoto(path)) = load_rx.recv(), if in_flight.len() < max_in_flight => {
                if in_flight.insert(path.clone()) {
                    tasks.spawn({
                        let p = path.clone();
                        async move {
                            let res = tokio::task::spawn_blocking(move || decode_rgba8_apply_exif(&p)).await;
                            (path, res.ok().and_then(|r| r.ok()))
                        }
                    });
                }
            }

            // Handle completed decodes as they finish
            Some(join_res) = tasks.join_next() => {
                if let Ok((path, maybe_img)) = join_res {
                    // remove from in-flight set
                    in_flight.remove(&path);
                    match maybe_img {
                        Some(rgba8) => {
                            debug!("loaded (rgba8): {}", path.display());
                            let (width, height) = rgba8.dimensions();
                            let prepared = PreparedImageCpu { path: path.clone(), width, height, pixels: rgba8.into_raw() };
                            let ready = ReadyPhoto {
                                path: path.clone(),
                                event: PhotoLoaded(prepared),
                            };
                            send_ready(ready, &mut last_sent_path, &mut pending_ready, &to_viewer).await;
                        }
                        None => {
                            debug!("invalid photo {}", path.display());
                            let _ = invalid_tx.send(InvalidPhoto(path)).await;
                        }
                    }
                }
            }

            else => {
                // If both channels are closed and nothing in flight, exit
                if in_flight.is_empty() {
                    flush_pending(&mut last_sent_path, &mut pending_ready, &to_viewer).await;
                    break;
                }
            }
        }
    }
    Ok(())
}

struct ReadyPhoto {
    path: PathBuf,
    event: PhotoLoaded,
}

async fn send_ready(
    ready: ReadyPhoto,
    last_sent: &mut Option<PathBuf>,
    pending: &mut Option<ReadyPhoto>,
    to_viewer: &Sender<PhotoLoaded>,
) {
    let mut current = Some(ready);
    while let Some(ReadyPhoto { path, event }) = current {
        if last_sent.as_ref() == Some(&path) {
            if pending.is_none() {
                *pending = Some(ReadyPhoto { path, event });
            } else {
                let _ = to_viewer.send(event).await;
                *last_sent = Some(path);
            }
            return;
        } else {
            let _ = to_viewer.send(event).await;
            *last_sent = Some(path);
            current = pending.take();
        }
    }
}

async fn flush_pending(
    last_sent: &mut Option<PathBuf>,
    pending: &mut Option<ReadyPhoto>,
    to_viewer: &Sender<PhotoLoaded>,
) {
    if let Some(ReadyPhoto { path, event }) = pending.take() {
        let _ = to_viewer.send(event).await;
        *last_sent = Some(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use tokio::sync::mpsc;

    fn ready_for(path: &str) -> ReadyPhoto {
        let path_buf = PathBuf::from(path);
        let prepared = PreparedImageCpu {
            path: path_buf.clone(),
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 0],
        };
        ReadyPhoto {
            path: path_buf,
            event: PhotoLoaded(prepared),
        }
    }

    // JPEG 2x1 with EXIF orientation 6 (rotate 90 CW), base64 encoded
    const ORIENT6_JPEG: &str = concat!(
        "/9j/4AAQSkZJRgABAQAAAQABAAD/4QAiRXhpZgAATU0AKgAAAAgAAQESAAMAAAABAAYAAAAAAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/",
        "2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAABAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDi6KKK+ZP3E//Z"
    );

    #[test]
    fn applies_orientation_six() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(ORIENT6_JPEG)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orient6.jpg");
        std::fs::write(&path, &bytes).unwrap();
        let img = decode_rgba8_apply_exif(&path).unwrap();
        assert_eq!(img.dimensions(), (1, 2));
    }

    #[tokio::test]
    async fn reorders_single_repeat_when_possible() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut last_sent = None;
        let mut pending = None;

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert_eq!(rx.recv().await.unwrap().0.path, PathBuf::from("a"));
        assert!(pending.is_none());

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert!(pending.is_some());
        assert!(rx.try_recv().is_err());

        send_ready(ready_for("b"), &mut last_sent, &mut pending, &tx).await;
        assert_eq!(rx.recv().await.unwrap().0.path, PathBuf::from("b"));
        assert_eq!(rx.recv().await.unwrap().0.path, PathBuf::from("a"));
        assert!(pending.is_none());
        assert_eq!(last_sent, Some(PathBuf::from("a")));
    }

    #[tokio::test]
    async fn flushes_pending_when_nothing_else_arrives() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut last_sent = None;
        let mut pending = None;

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert_eq!(rx.recv().await.unwrap().0.path, PathBuf::from("a"));

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert!(pending.is_some());
        assert!(rx.try_recv().is_err());

        flush_pending(&mut last_sent, &mut pending, &tx).await;
        assert_eq!(rx.recv().await.unwrap().0.path, PathBuf::from("a"));
        assert!(pending.is_none());
        assert_eq!(last_sent, Some(PathBuf::from("a")));
    }
}
