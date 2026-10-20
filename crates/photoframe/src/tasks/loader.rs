use crate::events::{InvalidPhoto, LoadPhoto, PhotoLoaded, PreparedImageCpu};
use anyhow::Result;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Seek};
use std::path::{Path, PathBuf};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::debug;

mod decode_scale;
#[cfg(test)]
pub(crate) mod scaled_decode_tests;
pub use decode_scale::{DecodeScaling, DecodeTarget, bg_sample_scale, decoded_suffices};

/// Upper bound on peak allocation while decoding a single image. On a
/// memory-constrained Pi a pathological image (e.g. a multi-gigapixel scan or
/// panorama) could otherwise OOM-kill the whole process. ~512 MiB comfortably
/// covers >100 MP photos while rejecting absurd inputs; an over-limit image
/// surfaces as a normal decode error and is skipped (never deleted).
const MAX_DECODE_ALLOC_BYTES: u64 = 512 * 1024 * 1024;

/// A JPEG decoded straight from DCT coefficients at a reduced scale.
struct ScaledJpeg {
    /// Cropped to `decode_scale::scaled_len` on each axis, in the file's
    /// (unrotated) orientation.
    img: image::RgbaImage,
    /// DCT scale as `n/8`; `1`, `2` or `4` (`8` is never routed here).
    idct: u8,
    /// Stored (unrotated) size of the source.
    source: (u16, u16),
}

/// An image ready for the viewer.
pub(crate) struct DecodedPhoto {
    /// Oriented pixels, possibly reduced (see `source_size`).
    pub image: image::RgbaImage,
    /// Size of the original photo in its displayed orientation.
    pub source_size: (u32, u32),
}

/// Allocation guard for the scaled path. The `image` path charges
/// `MAX_DECODE_ALLOC_BYTES` against the decoded buffer (width x height x
/// components); apply the same accounting to what jpeg-decoder holds, which is
/// that size again for a progressive stream (it keeps all coefficients at full
/// size, two bytes each). Over the bound the caller defers to the `image` path
/// rather than rejecting, so whether a photo is accepted never depends on the
/// scale chosen.
fn within_scaled_decode_bound(width: u16, height: u16, components: u64, progressive: bool) -> bool {
    let bytes_per_sample: u64 = if progressive { 2 } else { 1 };
    width as u64 * height as u64 * components * bytes_per_sample <= MAX_DECODE_ALLOC_BYTES
}

/// Try to decode a JPEG at the smallest DCT scale that covers `target`.
///
/// `None` means "use the `image` crate": the scale came out as full size,
/// the source is large enough that the `image` path should be the one to
/// accept or reject it, jpeg-decoder rejected the stream (CMYK, arithmetic
/// coding, 12-bit, ...), or its output did not cover the required size.
fn decode_jpeg_scaled<R: std::io::Read>(
    reader: R,
    orientation: u16,
    target: &DecodeTarget,
) -> Option<ScaledJpeg> {
    use jpeg_decoder::{CodingProcess, PixelFormat};

    let mut decoder = jpeg_decoder::Decoder::new(reader);
    decoder.set_max_decoding_buffer_size(MAX_DECODE_ALLOC_BYTES as usize);
    if let Err(err) = decoder.read_info() {
        debug!("jpeg-decoder rejected header, using image crate: {err}");
        return None;
    }
    let info = decoder.info()?;
    let components: u64 = match info.pixel_format {
        PixelFormat::RGB24 => 3,
        PixelFormat::L8 => 1,
        other => {
            debug!("jpeg pixel format {other:?} unsupported by scaled decode, using image crate");
            return None;
        }
    };
    let (file_w, file_h) = (info.width, info.height);

    let (idct, need_w, need_h) = decode_scale::plan(file_w, file_h, orientation, target);
    if idct >= 8 {
        return None;
    }

    let progressive = info.coding_process == CodingProcess::DctProgressive;
    if !within_scaled_decode_bound(file_w, file_h, components, progressive) {
        debug!("{file_w}x{file_h} jpeg over the scaled-decode memory bound, using image crate");
        return None;
    }

    // `scale` picks the smallest scale that covers *either* requested axis, so
    // pass the exact output width of the scale chosen above and an
    // unsatisfiable height: the width alone then decides, and the returned
    // size says what we actually got.
    let expected_w = decode_scale::dct_len(file_w, idct);
    let request_w = expected_w.min(u16::MAX as u32) as u16;
    let (out_w, out_h) = match decoder.scale(request_w, u16::MAX) {
        Ok(size) => size,
        Err(err) => {
            debug!("jpeg-decoder scale failed, using image crate: {err}");
            return None;
        }
    };
    // jpeg-decoder pads the last partial block, so its output is ceil(len*n/8)
    // pixels of which the last may be replicated edge. Keep only the exact
    // scaled extent so the buffer spans the photo and nothing more.
    let (w, h) = (
        decode_scale::scaled_len(file_w, idct),
        decode_scale::scaled_len(file_h, idct),
    );
    let got = (out_w as u32, out_h as u32);
    if got
        != (
            decode_scale::dct_len(file_w, idct),
            decode_scale::dct_len(file_h, idct),
        )
        || w > got.0
        || h > got.1
        || w < need_w
        || h < need_h
    {
        // Not the scale we planned, or it would undershoot the rect: decode in
        // full instead.
        debug!("scaled jpeg {got:?} unusable for {need_w}x{need_h}, using image crate");
        return None;
    }

    let pixels = match decoder.decode() {
        Ok(pixels) => pixels,
        Err(err) => {
            debug!("jpeg-decoder failed, using image crate: {err}");
            return None;
        }
    };
    let stride = got.0 as usize;
    let comps = components as usize;
    if pixels.len() != stride * got.1 as usize * comps {
        return None;
    }
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for row in pixels.chunks_exact(stride * comps).take(h as usize) {
        let row = &row[..w as usize * comps];
        if comps == 3 {
            for px in row.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        } else {
            for &v in row {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    Some(ScaledJpeg {
        img,
        idct,
        source: (file_w, file_h),
    })
}

// Decodes an image to RGBA8 and applies EXIF orientation if available.
//
// Model: the viewer resamples every decoded photo into a rect no larger than
// its canvas (the screen times the oversample factor), so pixels decoded
// beyond what that resample can use buy nothing. For JPEGs, `scaling` names
// that canvas and the photo is decoded straight from the DCT at the smallest
// of the 1/8, 1/4, 1/2 and full scales whose output still covers the largest
// rect the photo can be drawn into, on *both* axes, measured in the displayed
// orientation (EXIF rotations of 90/270 swap the axes first). The covering
// rect is the contained fit, or the cover fit when `fill-when-fits` is
// configured, or the blurred-background sample, whichever is largest (see
// `decode_scale`). jpeg-decoder pads its output to whole blocks, so the
// buffer is cropped to the exact scaled extent (`len * n / 8`, rounded), which
// keeps its aspect and edges true to the photo. The result is at most the
// size of a full decode; `DecodedPhoto::source_size` carries the original
// photo's size for everything that reasons about the photo itself (fill
// eligibility, and the matting worker's check that a reduced decode still
// covers the canvas it is drawn into, which may have changed since).
//
// Fallbacks, all of which are a plain full-size `image` crate decode:
//   - `scaling` is `None` (canvas not yet known, or photo effects are on and
//     work in source pixels);
//   - the file is not a JPEG, or the smallest covering scale is full size;
//   - jpeg-decoder rejects the stream (CMYK, arithmetic coding, 12-bit, ...),
//     fails mid-decode, or its output would not cover the required size;
//   - the source is over the allocation bound as jpeg-decoder would hold it:
//     the `image` path then accepts or rejects it by its own accounting, so
//     acceptance does not depend on the scale.
//
// Orientation handling is a best-effort; if metadata is missing, the original
// orientation is preserved. The file is opened only once: EXIF is read first, then
// the reader is seeked back to the start for image decoding. EXIF orientation
// is applied after decode, so it operates on the already-reduced buffer.
pub(crate) fn decode_rgba8_apply_exif(
    path: &Path,
    scaling: Option<DecodeTarget>,
) -> anyhow::Result<DecodedPhoto> {
    let file = File::open(path)?;
    let mut buf = BufReader::new(file);

    // Read EXIF orientation from the already-open handle.
    let orientation: u16 = (|| -> Option<u16> {
        let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
        let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
        let val = field.value.get_uint(0)? as u16;
        debug!("exif orientation {} for {}", val, path.display());
        Some(val)
    })()
    .unwrap_or(1);

    // Seek back to the start so the image decoder reads from the beginning.
    buf.seek(std::io::SeekFrom::Start(0))?;

    let reader = image::ImageReader::new(buf).with_guessed_format()?;
    let format = reader.format();
    let mut buf = reader.into_inner();

    let mut scaled: Option<(image::RgbaImage, (u16, u16))> = None;
    if let (Some(target), Some(image::ImageFormat::Jpeg)) = (scaling, format) {
        if let Some(jpeg) = decode_jpeg_scaled(&mut buf, orientation, &target) {
            debug!(
                path = %path.display(),
                scale = %format!("{}/8", jpeg.idct),
                source_w = jpeg.source.0,
                source_h = jpeg.source.1,
                decoded_w = jpeg.img.width(),
                decoded_h = jpeg.img.height(),
                canvas_w = target.canvas_w,
                canvas_h = target.canvas_h,
                "jpeg_scaled_decode"
            );
            scaled = Some((jpeg.img, jpeg.source));
        } else {
            buf.seek(std::io::SeekFrom::Start(0))?;
        }
    }

    let (mut img, stored_source) = match scaled {
        Some((img, source)) => (img, Some(source)),
        None => {
            let mut reader = image::ImageReader::new(buf).with_guessed_format()?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
            reader.limits(limits);
            (reader.decode()?.to_rgba8(), None)
        }
    };
    // Original size in the stored orientation, before any EXIF rotation.
    let stored_source = stored_source
        .map(|(w, h)| (w as u32, h as u32))
        .unwrap_or_else(|| img.dimensions());

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

    let source_size = if decode_scale::orientation_swaps_axes(orientation) {
        (stored_source.1, stored_source.0)
    } else {
        stored_source
    };
    Ok(DecodedPhoto {
        image: img,
        source_size,
    })
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
    scaling: DecodeScaling,
) -> Result<()> {
    let mut in_flight: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let mut priority_inflight: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    // Each decode carries the sequence number it was requested in, so results can
    // be emitted in request order even though they finish out of order.
    let mut tasks: JoinSet<(u64, std::path::PathBuf, Option<DecodedPhoto>)> = JoinSet::new();
    let mut next_seq: u64 = 0;
    let mut reorder = ReorderBuffer::new();
    let mut pending_ready: Option<ReadyPhoto> = None;
    let mut last_sent_path: Option<PathBuf> = None;

    loop {
        // Bound outstanding work (in-flight + buffered) so a slow decode applies
        // backpressure instead of letting the reorder buffer grow without limit.
        let can_accept = next_seq.saturating_sub(reorder.next_emit()) < max_in_flight as u64;

        select! {
            _ = cancel.cancelled() => {
                flush_pending(&mut last_sent_path, &mut pending_ready, &to_viewer).await;
                break;
            },

            // Accept new load requests while the outstanding window has room.
            Some(LoadPhoto { path, priority }) = load_rx.recv(), if can_accept => {
                if priority {
                    priority_inflight.insert(path.clone());
                }
                if in_flight.insert(path.clone()) {
                    let seq = next_seq;
                    next_seq += 1;
                    tasks.spawn({
                        let p = path.clone();
                        let scaling = scaling.clone();
                        async move {
                            let target = scaling.target();
                            let res = tokio::task::spawn_blocking(move || decode_rgba8_apply_exif(&p, target)).await;
                            (seq, path, res.ok().and_then(|r| r.ok()))
                        }
                    });
                }
                // A duplicate of an already in-flight path is dropped (no seq used);
                // any priority upgrade was recorded above.
            }

            // Handle completed decodes as they finish, then release in request order.
            Some(join_res) = tasks.join_next() => {
                if let Ok((seq, path, maybe_img)) = join_res {
                    in_flight.remove(&path);
                    let priority = priority_inflight.remove(&path);
                    match maybe_img {
                        Some(DecodedPhoto { image: rgba8, source_size }) => {
                            debug!("loaded (rgba8): {}", path.display());
                            let (width, height) = rgba8.dimensions();
                            let prepared = PreparedImageCpu { path: path.clone(), width, height, source_size, pixels: rgba8.into_raw() };
                            let event = PhotoLoaded { prepared, priority };
                            reorder.insert(seq, Some(ReadyPhoto { path, event }));
                        }
                        None => {
                            debug!("invalid photo {}", path.display());
                            let _ = invalid_tx.send(InvalidPhoto(path)).await;
                            // Mark the slot done so emission can advance past it.
                            reorder.insert(seq, None);
                        }
                    }
                    for ready in reorder.drain_ready() {
                        send_ready(ready, &mut last_sent_path, &mut pending_ready, &to_viewer).await;
                    }
                }
            }

            else => {
                // Channels closed and nothing in flight: drain, flush, exit.
                if in_flight.is_empty() {
                    for ready in reorder.drain_ready() {
                        send_ready(ready, &mut last_sent_path, &mut pending_ready, &to_viewer).await;
                    }
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

/// Buffers decoded photos that finished out of order and releases them in the
/// order they were requested. A `None` slot marks a decode that failed (the photo
/// was sent to `invalid_tx`), so emission can advance past it.
struct ReorderBuffer {
    next_emit: u64,
    slots: BTreeMap<u64, Option<ReadyPhoto>>,
}

impl ReorderBuffer {
    fn new() -> Self {
        Self {
            next_emit: 0,
            slots: BTreeMap::new(),
        }
    }

    fn next_emit(&self) -> u64 {
        self.next_emit
    }

    fn insert(&mut self, seq: u64, slot: Option<ReadyPhoto>) {
        self.slots.insert(seq, slot);
    }

    /// Remove and return the next contiguous run of ready photos, in request
    /// order, skipping failed slots.
    fn drain_ready(&mut self) -> Vec<ReadyPhoto> {
        let mut out = Vec::new();
        while let Some(slot) = self.slots.remove(&self.next_emit) {
            self.next_emit += 1;
            if let Some(ready) = slot {
                out.push(ready);
            }
        }
        out
    }
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
            source_size: (1, 1),
            pixels: vec![0, 0, 0, 0],
        };
        ReadyPhoto {
            path: path_buf,
            event: PhotoLoaded {
                prepared,
                priority: false,
            },
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
        let img = decode_rgba8_apply_exif(&path, None).unwrap().image;
        assert_eq!(img.dimensions(), (1, 2));
    }

    #[tokio::test]
    async fn reorders_single_repeat_when_possible() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut last_sent = None;
        let mut pending = None;

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        let first = rx.recv().await.unwrap();
        assert_eq!(first.prepared.path, PathBuf::from("a"));
        assert!(!first.priority);
        assert!(pending.is_none());

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert!(pending.is_some());
        assert!(rx.try_recv().is_err());

        send_ready(ready_for("b"), &mut last_sent, &mut pending, &tx).await;
        let second = rx.recv().await.unwrap();
        assert_eq!(second.prepared.path, PathBuf::from("b"));
        assert!(!second.priority);
        let third = rx.recv().await.unwrap();
        assert_eq!(third.prepared.path, PathBuf::from("a"));
        assert!(!third.priority);
        assert!(pending.is_none());
        assert_eq!(last_sent, Some(PathBuf::from("a")));
    }

    #[tokio::test]
    async fn flushes_pending_when_nothing_else_arrives() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut last_sent = None;
        let mut pending = None;

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert_eq!(rx.recv().await.unwrap().prepared.path, PathBuf::from("a"));

        send_ready(ready_for("a"), &mut last_sent, &mut pending, &tx).await;
        assert!(pending.is_some());
        assert!(rx.try_recv().is_err());

        flush_pending(&mut last_sent, &mut pending, &tx).await;
        let flushed = rx.recv().await.unwrap();
        assert_eq!(flushed.prepared.path, PathBuf::from("a"));
        assert!(!flushed.priority);
        assert!(pending.is_none());
        assert_eq!(last_sent, Some(PathBuf::from("a")));
    }

    fn ready_paths(items: &[ReadyPhoto]) -> Vec<String> {
        items
            .iter()
            .map(|r| r.path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn reorder_buffer_emits_in_request_order_skipping_invalid() {
        let mut buf = ReorderBuffer::new();

        // seq 2 finishes first — nothing emits until seq 0 arrives.
        buf.insert(2, Some(ready_for("c")));
        assert!(ready_paths(&buf.drain_ready()).is_empty());

        // seq 0 arrives — emit only 0 (still waiting on 1).
        buf.insert(0, Some(ready_for("a")));
        assert_eq!(ready_paths(&buf.drain_ready()), ["a"]);

        // seq 1 failed (invalid) — skip it and release the buffered seq 2.
        buf.insert(1, None);
        assert_eq!(ready_paths(&buf.drain_ready()), ["c"]);

        // seq 3 arrives in order.
        buf.insert(3, Some(ready_for("d")));
        assert_eq!(ready_paths(&buf.drain_ready()), ["d"]);

        assert_eq!(buf.next_emit(), 4);
    }
}
