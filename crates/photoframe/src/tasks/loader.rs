use crate::events::{
    InvalidPhoto, InventoryEvent, LoadPhoto, LoadedPartner, MetaSource, PhotoLoaded,
    PreparedImageCpu,
};
use anyhow::Result;
use std::collections::{BTreeMap, HashSet};
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
pub use decode_scale::{
    DecodeScaling, DecodeTarget, bg_sample_scale, decoded_suffices, orientation_swaps_axes,
};

/// Upper bound on peak allocation while decoding a single image. On a
/// memory-constrained Pi a pathological image (e.g. a multi-gigapixel scan or
/// panorama) could otherwise OOM-kill the whole process. ~512 MiB comfortably
/// covers >100 MP photos while rejecting absurd inputs; an over-limit image
/// surfaces as a normal decode error and is skipped (never deleted).
const MAX_DECODE_ALLOC_BYTES: u64 = 512 * 1024 * 1024;

/// A JPEG decoded straight from DCT coefficients at a reduced scale.
struct ScaledJpeg {
    /// `decode_scale::scaled_len` on each axis, spanning exactly the photo,
    /// in the file's (unrotated) orientation.
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

/// Resample `img` so that its first `extent` pixels (fractional) fill an
/// `out_w` x `out_h` image exactly: drops the padding past the photo's true
/// edge and removes the sub-pixel mismatch that rounding to a whole pixel
/// would leave. The factor is within one pixel of 1:1.
fn resample_to_extent(
    img: image::RgbaImage,
    extent: (f64, f64),
    out: (u32, u32),
) -> Option<image::RgbaImage> {
    use fast_image_resize as fir;
    let (src_w, src_h) = img.dimensions();
    let src =
        fir::images::Image::from_vec_u8(src_w, src_h, img.into_raw(), fir::PixelType::U8x4).ok()?;
    let mut dst = fir::images::Image::new(out.0, out.1, fir::PixelType::U8x4);
    let options = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom))
        .crop(0.0, 0.0, extent.0, extent.1);
    fir::Resizer::new().resize(&src, &mut dst, &options).ok()?;
    image::RgbaImage::from_raw(out.0, out.1, dst.into_vec())
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
    // jpeg-decoder pads the last partial block, so its output is
    // ceil(len*n/8) pixels covering the photo's exact extent len*n/8 plus a
    // sliver of replicated edge. The viewer resamples the whole buffer as the
    // whole photo, so the buffer must span exactly that extent.
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
    let comps = components as usize;
    if pixels.len() != got.0 as usize * got.1 as usize * comps {
        return None;
    }
    let mut rgba = Vec::with_capacity(got.0 as usize * got.1 as usize * 4);
    if comps == 3 {
        for px in pixels.chunks_exact(3) {
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    } else {
        for &v in &pixels {
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let padded = image::RgbaImage::from_raw(got.0, got.1, rgba)?;
    let img = if file_w as u32 * idct as u32 % 8 == 0 && file_h as u32 * idct as u32 % 8 == 0 {
        // Exact multiple of the block size: nothing was padded.
        padded
    } else {
        let extent = (
            file_w as f64 * idct as f64 / 8.0,
            file_h as f64 * idct as f64 / 8.0,
        );
        resample_to_extent(padded, extent, (w, h))?
    };
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
// `decode_scale`). jpeg-decoder pads its output to whole blocks, so when the
// scaled extent (`len * n / 8`) is fractional the buffer is resampled, near
// 1:1, to its rounded length so that it spans exactly the photo: the viewer
// treats the whole buffer as the whole photo. The result is at most the
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

    // Read EXIF orientation from the already-open handle (the same read the
    // header pass uses, so the two agree).
    let orientation = crate::processing::header::read_exif_orientation(&mut buf, path);

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

    let source_size = crate::processing::header::displayed_dimensions(stored_source, orientation);
    Ok(DecodedPhoto {
        image: img,
        source_size,
    })
}

/// Very simple loader:
/// - Reads the bytes (to prove existence) and forwards a `PreparedPhoto`.
/// - On I/O error, emits `InvalidPhoto`.
/// - With portrait pairing enabled (`meta_tx` is `Some`), reports every
///   successful decode's displayed size to the inventory as
///   `PhotoMeta { source: Decoded }`, the authority for pairability.
pub async fn run(
    mut load_rx: Receiver<LoadPhoto>,
    invalid_tx: Sender<InvalidPhoto>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
    max_in_flight: usize,
    scaling: DecodeScaling,
    meta_tx: Option<Sender<InventoryEvent>>,
) -> Result<()> {
    let mut in_flight: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let mut priority_inflight: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    // Each slot (one screen: a photo, or a pair's two decodes joined) carries
    // the sequence number it was requested in, so results can be emitted in
    // request order even though they finish out of order.
    let mut tasks: JoinSet<DecodedSlot> = JoinSet::new();
    let mut next_seq: u64 = 0;
    let mut reorder = ReorderBuffer::new();
    let mut pending_ready: Option<ReadyPhoto> = None;
    let mut last_sent_path: Option<Vec<PathBuf>> = None;
    // Decodes per outstanding slot (1, or 2 for a pair), and a pair request
    // waiting for room: `loader-max-concurrent-decodes` bounds decodes, not
    // slots, so a pair takes two places.
    let mut slot_decodes: BTreeMap<u64, usize> = BTreeMap::new();
    let mut waiting: Option<LoadPhoto> = None;

    loop {
        // Bound outstanding work (in-flight + buffered decodes) so a slow decode
        // applies backpressure instead of letting the reorder buffer grow
        // without limit. With one decode per slot this is the slot count.
        slot_decodes.retain(|seq, _| *seq >= reorder.next_emit());
        let outstanding: usize = slot_decodes.values().sum();
        if let Some(request) = waiting.take() {
            if has_room(outstanding, request_decodes(&request), max_in_flight) {
                start_slot(
                    request,
                    &mut StartSlot {
                        in_flight: &mut in_flight,
                        priority_inflight: &mut priority_inflight,
                        tasks: &mut tasks,
                        next_seq: &mut next_seq,
                        slot_decodes: &mut slot_decodes,
                        scaling: &scaling,
                    },
                );
                continue;
            }
            waiting = Some(request);
        }
        let can_accept = waiting.is_none() && outstanding < max_in_flight;

        select! {
            _ = cancel.cancelled() => {
                flush_pending(&mut last_sent_path, &mut pending_ready, &to_viewer).await;
                break;
            },

            // Accept new load requests while the outstanding window has room.
            Some(request) = load_rx.recv(), if can_accept => {
                // A pair that would exceed the decode bound waits for room.
                waiting = Some(request);
            }

            // Handle completed decodes as they finish, then release in request order.
            Some(join_res) = tasks.join_next() => {
                if let Ok((seq, decoded)) = join_res {
                    let mut shown = Vec::with_capacity(decoded.len());
                    for (path, maybe_img) in decoded {
                        in_flight.remove(&path);
                        let priority = priority_inflight.remove(&path);
                        match maybe_img {
                            Some(DecodedPhoto { image: rgba8, source_size }) => {
                                debug!("loaded (rgba8): {}", path.display());
                                send_decoded_meta(meta_tx.as_ref(), &path, source_size).await;
                                let (width, height) = rgba8.dimensions();
                                let prepared = PreparedImageCpu { path, width, height, source_size, pixels: rgba8.into_raw() };
                                shown.push((prepared, priority));
                            }
                            None => {
                                debug!("invalid photo {}", path.display());
                                let _ = invalid_tx.send(InvalidPhoto(path)).await;
                            }
                        }
                    }
                    // A failed member is dropped and the other is shown solo; an
                    // empty slot is marked done so emission can advance past it.
                    reorder.insert(seq, ready_slot(shown));
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

/// `PhotoMeta::Decoded` for one successful decode; a no-op unless pairing
/// is enabled (`meta_tx` is `Some`).
async fn send_decoded_meta(
    meta_tx: Option<&Sender<InventoryEvent>>,
    path: &Path,
    source_size: (u32, u32),
) {
    if let Some(tx) = meta_tx {
        let _ = tx
            .send(InventoryEvent::PhotoMeta {
                path: path.to_path_buf(),
                display_size: source_size,
                source: MetaSource::Decoded,
            })
            .await;
    }
}

/// Decodes a request asks for: one per member.
fn request_decodes(request: &LoadPhoto) -> usize {
    1 + usize::from(request.partner.is_some())
}

/// Whether `decodes` more fit under `max` with `outstanding` decodes in
/// flight or buffered. A request always fits an idle loader, so a pair still
/// runs with `loader-max-concurrent-decodes: 1`.
fn has_room(outstanding: usize, decodes: usize, max: usize) -> bool {
    outstanding == 0 || outstanding + decodes <= max
}

struct StartSlot<'a> {
    in_flight: &'a mut HashSet<PathBuf>,
    priority_inflight: &'a mut HashSet<PathBuf>,
    tasks: &'a mut JoinSet<DecodedSlot>,
    next_seq: &'a mut u64,
    slot_decodes: &'a mut BTreeMap<u64, usize>,
    scaling: &'a DecodeScaling,
}

/// Admit `request`'s members (per-member dedupe) and decode them as one
/// slot. A request whose every member is already in flight is dropped (no
/// seq used); any priority upgrade was recorded.
fn start_slot(request: LoadPhoto, st: &mut StartSlot<'_>) {
    let members = admit_members(request, st.in_flight, st.priority_inflight);
    if members.is_empty() {
        return;
    }
    let seq = *st.next_seq;
    *st.next_seq += 1;
    st.slot_decodes.insert(seq, members.len());
    let scaling = st.scaling.clone();
    // A pair slot runs its members' decodes concurrently, joined under one
    // reorder seq.
    st.tasks.spawn(async move {
        let decodes = members.into_iter().map(|path| {
            let target = scaling.target();
            async move {
                let p = path.clone();
                let res =
                    tokio::task::spawn_blocking(move || decode_rgba8_apply_exif(&p, target)).await;
                (path, res.ok().and_then(|r| r.ok()))
            }
        });
        (seq, futures::future::join_all(decodes).await)
    });
}

/// One finished slot: its reorder seq and each member's decode, in request
/// order.
type DecodedSlot = (u64, Vec<(PathBuf, Option<DecodedPhoto>)>);

/// **Loader dedupe (B8), per member.** Record each member's priority, and
/// keep the members whose path is not already being decoded: a pair whose
/// member is in flight degrades to the other member, and is never dropped
/// whole. Today's solo dedupe is the case of one member.
fn admit_members(
    request: LoadPhoto,
    in_flight: &mut HashSet<PathBuf>,
    priority_inflight: &mut HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let LoadPhoto {
        path,
        priority,
        partner,
    } = request;
    std::iter::once((path, priority))
        .chain(partner.map(|partner| (partner.path, partner.priority)))
        .filter_map(|(path, priority)| {
            if priority {
                priority_inflight.insert(path.clone());
            }
            in_flight.insert(path.clone()).then_some(path)
        })
        .collect()
}

/// The viewer event for one slot's decoded members, in request order: the
/// first (left), and a partner when both decoded. `None` when none did.
fn ready_slot(shown: Vec<(PreparedImageCpu, bool)>) -> Option<ReadyPhoto> {
    let mut shown = shown.into_iter();
    let (prepared, priority) = shown.next()?;
    let partner = shown
        .next()
        .map(|(prepared, priority)| LoadedPartner { prepared, priority });
    Some(ReadyPhoto {
        members: std::iter::once(prepared.path.clone())
            .chain(partner.as_ref().map(|p| p.prepared.path.clone()))
            .collect(),
        event: PhotoLoaded {
            prepared,
            priority,
            partner,
        },
    })
}

struct ReadyPhoto {
    /// The slot's photos (one, or a pair's two), for the back-to-back guard.
    members: Vec<PathBuf>,
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

/// The back-to-back guard: a slot sharing a photo with the slot sent just
/// before it is held back one slot (if nothing is held yet), so a photo is
/// not on two consecutive screens. Keyed on member sets, so it also catches
/// a pair whose second member was on the previous screen (review F5).
async fn send_ready(
    ready: ReadyPhoto,
    last_sent: &mut Option<Vec<PathBuf>>,
    pending: &mut Option<ReadyPhoto>,
    to_viewer: &Sender<PhotoLoaded>,
) {
    let shares = |last: &Option<Vec<PathBuf>>, members: &[PathBuf]| {
        last.as_ref()
            .is_some_and(|last| members.iter().any(|m| last.contains(m)))
    };
    let mut current = Some(ready);
    while let Some(ReadyPhoto { members, event }) = current {
        if shares(last_sent, &members) {
            if pending.is_none() {
                *pending = Some(ReadyPhoto { members, event });
            } else {
                let _ = to_viewer.send(event).await;
                *last_sent = Some(members);
            }
            return;
        } else {
            let _ = to_viewer.send(event).await;
            *last_sent = Some(members);
            current = pending.take();
        }
    }
}

async fn flush_pending(
    last_sent: &mut Option<Vec<PathBuf>>,
    pending: &mut Option<ReadyPhoto>,
    to_viewer: &Sender<PhotoLoaded>,
) {
    if let Some(ReadyPhoto { members, event }) = pending.take() {
        let _ = to_viewer.send(event).await;
        *last_sent = Some(members);
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
            members: vec![path_buf],
            event: PhotoLoaded {
                prepared,
                priority: false,
                partner: None,
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
        assert_eq!(last_sent, Some(vec![PathBuf::from("a")]));
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
        assert_eq!(last_sent, Some(vec![PathBuf::from("a")]));
    }

    fn ready_paths(items: &[ReadyPhoto]) -> Vec<String> {
        items
            .iter()
            .map(|r| r.members[0].to_string_lossy().into_owned())
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

    /// Load one photo through `run` and return what reached the viewer and
    /// the inventory.
    async fn load_once(meta: bool) -> (PhotoLoaded, Vec<InventoryEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.png");
        image::RgbaImage::from_pixel(30, 40, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let (load_tx, load_rx) = mpsc::channel(4);
        let (invalid_tx, _invalid_rx) = mpsc::channel(4);
        let (viewer_tx, mut viewer_rx) = mpsc::channel(4);
        let (meta_tx, mut meta_rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            load_rx,
            invalid_tx,
            viewer_tx,
            cancel.clone(),
            2,
            DecodeScaling::disabled(),
            meta.then_some(meta_tx),
        ));
        load_tx
            .send(LoadPhoto {
                path: path.clone(),
                priority: false,
                partner: None,
            })
            .await
            .unwrap();
        let loaded = tokio::time::timeout(std::time::Duration::from_secs(5), viewer_rx.recv())
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        let _ = task.await;
        let mut metas = Vec::new();
        while let Ok(event) = meta_rx.try_recv() {
            metas.push(event);
        }
        (loaded, metas)
    }

    #[tokio::test]
    async fn decoded_meta_is_sent_only_when_pairing_is_enabled() {
        let (loaded, metas) = load_once(true).await;
        assert_eq!(loaded.prepared.source_size, (30, 40));
        match metas.as_slice() {
            [
                InventoryEvent::PhotoMeta {
                    path,
                    display_size,
                    source,
                },
            ] => {
                assert_eq!(path, &loaded.prepared.path);
                assert_eq!(*display_size, (30, 40));
                assert_eq!(*source, MetaSource::Decoded);
            }
            other => panic!("expected one Decoded meta, got {other:?}"),
        }
        let (_, metas) = load_once(false).await;
        assert!(metas.is_empty(), "{metas:?}");
    }

    #[test]
    fn b8_dedupe_is_per_member_and_degrades_a_pair() {
        use crate::events::LoadPartner;
        let mut in_flight = HashSet::new();
        let mut priority = HashSet::new();
        let pair = |a: &str, b: &str, b_priority: bool| LoadPhoto {
            path: PathBuf::from(a),
            priority: false,
            partner: Some(LoadPartner {
                path: PathBuf::from(b),
                priority: b_priority,
            }),
        };
        // b is already being decoded: the pair (a, b) decodes as a alone.
        in_flight.insert(PathBuf::from("b"));
        let members = admit_members(pair("a", "b", true), &mut in_flight, &mut priority);
        assert_eq!(members, [PathBuf::from("a")]);
        // The in-flight member's priority upgrade is still recorded.
        assert!(priority.contains(Path::new("b")));
        // Its first member in flight: (a, c) decodes as c alone.
        let members = admit_members(pair("a", "c", false), &mut in_flight, &mut priority);
        assert_eq!(members, [PathBuf::from("c")]);
        // Both in flight: nothing to decode, no slot.
        assert!(admit_members(pair("a", "c", false), &mut in_flight, &mut priority).is_empty());
        // A fresh pair keeps its order.
        let members = admit_members(pair("d", "e", false), &mut in_flight, &mut priority);
        assert_eq!(members, [PathBuf::from("d"), PathBuf::from("e")]);
    }

    async fn recv_loaded(rx: &mut mpsc::Receiver<PhotoLoaded>) -> PhotoLoaded {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn a_pair_decodes_into_one_slot_and_a_failed_member_leaves_the_other_solo() {
        use crate::events::LoadPartner;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        image::RgbaImage::from_pixel(30, 40, image::Rgba([1, 2, 3, 255]))
            .save(&a)
            .unwrap();
        image::RgbaImage::from_pixel(20, 40, image::Rgba([4, 5, 6, 255]))
            .save(&b)
            .unwrap();
        let c = dir.path().join("c.png");
        image::RgbaImage::from_pixel(20, 40, image::Rgba([7, 8, 9, 255]))
            .save(&c)
            .unwrap();
        let missing = dir.path().join("missing.jpg");
        let (load_tx, load_rx) = mpsc::channel(4);
        let (invalid_tx, mut invalid_rx) = mpsc::channel(4);
        let (viewer_tx, mut viewer_rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            load_rx,
            invalid_tx,
            viewer_tx,
            cancel.clone(),
            4,
            DecodeScaling::disabled(),
            None,
        ));
        load_tx
            .send(LoadPhoto {
                path: a.clone(),
                priority: false,
                partner: Some(LoadPartner {
                    path: b.clone(),
                    priority: true,
                }),
            })
            .await
            .unwrap();
        let pair = recv_loaded(&mut viewer_rx).await;
        assert_eq!(pair.prepared.path, a);
        assert!(!pair.priority);
        let partner = pair.partner.expect("both members decoded");
        assert_eq!(partner.prepared.path, b);
        assert!(partner.priority, "each member keeps its own priority");

        load_tx
            .send(LoadPhoto {
                path: missing.clone(),
                priority: false,
                partner: Some(LoadPartner {
                    path: c.clone(),
                    priority: false,
                }),
            })
            .await
            .unwrap();
        let solo = recv_loaded(&mut viewer_rx).await;
        assert_eq!(solo.prepared.path, c, "the decoded member is shown solo");
        assert!(solo.partner.is_none());
        let InvalidPhoto(invalid) = invalid_rx.recv().await.unwrap();
        assert_eq!(invalid, missing);
        cancel.cancel();
        let _ = task.await;
    }

    fn ready_pair(a: &str, b: &str) -> ReadyPhoto {
        let mut ready = ready_for(a);
        let partner = ready_for(b);
        ready.members.push(PathBuf::from(b));
        ready.event.partner = Some(LoadedPartner {
            prepared: partner.event.prepared,
            priority: false,
        });
        ready
    }

    /// Review F5: the guard is keyed on member sets, so (p2, p4) then
    /// (p4, p5) holds the second screen back, as p4, p4 is held solo.
    #[tokio::test]
    async fn back_to_back_guard_sees_every_member_of_a_pair() {
        let (tx, mut rx) = mpsc::channel(4);
        let (mut last_sent, mut pending) = (None, None);
        send_ready(ready_pair("p2", "p4"), &mut last_sent, &mut pending, &tx).await;
        send_ready(ready_pair("p4", "p5"), &mut last_sent, &mut pending, &tx).await;
        assert!(pending.is_some(), "held: p4 was on the previous screen");
        send_ready(ready_for("p6"), &mut last_sent, &mut pending, &tx).await;
        let order: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| e.prepared.path)
            .collect();
        assert_eq!(
            order,
            [
                PathBuf::from("p2"),
                PathBuf::from("p6"),
                PathBuf::from("p4")
            ]
        );
    }

    /// Review F7: `loader-max-concurrent-decodes` bounds decodes; a pair takes
    /// two places, and an idle loader always takes a request.
    #[test]
    fn a_pair_counts_as_two_decodes() {
        assert!(
            has_room(0, 2, 1),
            "an idle loader runs a pair even at max 1"
        );
        assert!(has_room(1, 1, 2));
        assert!(!has_room(1, 2, 2), "a pair waits for a second free place");
        assert!(has_room(2, 2, 4));
        assert!(!has_room(3, 2, 4));
    }

    #[tokio::test]
    async fn pairs_run_at_one_concurrent_decode_without_stalling() {
        use crate::events::LoadPartner;
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for name in ["a", "b", "c", "d"] {
            let path = dir.path().join(format!("{name}.png"));
            image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
                .save(&path)
                .unwrap();
            paths.push(path);
        }
        let (load_tx, load_rx) = mpsc::channel(4);
        let (invalid_tx, _invalid_rx) = mpsc::channel(4);
        let (viewer_tx, mut viewer_rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            load_rx,
            invalid_tx,
            viewer_tx,
            cancel.clone(),
            1,
            DecodeScaling::disabled(),
            None,
        ));
        for pair in paths.chunks(2) {
            load_tx
                .send(LoadPhoto {
                    path: pair[0].clone(),
                    priority: false,
                    partner: Some(LoadPartner {
                        path: pair[1].clone(),
                        priority: false,
                    }),
                })
                .await
                .unwrap();
        }
        for pair in paths.chunks(2) {
            let loaded = recv_loaded(&mut viewer_rx).await;
            assert_eq!(loaded.prepared.path, pair[0]);
            assert_eq!(loaded.partner.unwrap().prepared.path, pair[1]);
        }
        cancel.cancel();
        let _ = task.await;
    }
}
