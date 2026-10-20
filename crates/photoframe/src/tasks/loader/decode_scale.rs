//! Planning for DCT-domain scaled JPEG decode.
//!
//! The viewer resamples every decoded photo into a rect no larger than its
//! canvas, so pixels decoded beyond what that resample can use are wasted. A
//! JPEG can be reconstructed at 1/2, 1/4 or 1/8 of its size directly from the
//! DCT coefficients (cheaper than decoding in full and resizing), and this
//! module decides the smallest of those scales that still *covers* the largest
//! rect the photo can be drawn into.
//!
//! The canvas size is only known once the viewer has a surface (it depends on
//! the compositor's mode and the GPU's texture limit), so the viewer publishes
//! it through [`DecodeScaling`] and the loader reads it per decode. Until it is
//! published, or when scaling would change what the viewer sees, the loader
//! decodes at full size exactly as before.

use crate::config::{Configuration, MattingConfig, MattingMode};
use std::sync::{Arc, Mutex};

/// JPEG DCT output scales are expressed as `n/8` for `n` in this list
/// (smallest first). `8` is a full-size decode.
const IDCT_SIZES: [u8; 4] = [1, 2, 4, 8];

/// Static requirements derived from configuration: which renderings of a photo
/// the viewer may produce besides the plain contained photo.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DecodeNeeds {
    /// `fill-when-fits` is configured: a photo may be cover-cropped to the full
    /// canvas, and the eligibility check reads the decoded dimensions, so the
    /// decode must cover the cover rect to leave both unchanged.
    cover: bool,
    /// Largest fraction of the canvas that a blurred background samples its
    /// cover-cropped copy of the photo at (1.0 when a background is not
    /// downsampled before blurring). `None` when no mat draws one.
    bg_sample_scale: Option<f32>,
}

/// The fraction of the canvas at which `style` samples a cover-cropped copy of
/// the photo for its background, or `None` when the mat draws no such
/// background. Mirrors the viewer: the sample is only shrunk when the blur
/// actually runs, and the scale is clamped to `0.01..=1`.
pub fn bg_sample_scale(style: &MattingMode) -> Option<f32> {
    match style {
        MattingMode::Blur {
            sigma,
            sample_scale,
            ..
        }
        | MattingMode::CinematicBlur {
            sigma,
            sample_scale,
            ..
        } => Some(if *sigma > 0.0 {
            let scale = if sample_scale.is_finite() {
                *sample_scale
            } else {
                MattingMode::default_blur_sample_scale()
            };
            scale.clamp(0.01, 1.0)
        } else {
            1.0
        }),
        _ => None,
    }
}

impl DecodeNeeds {
    fn from_matting(matting: &MattingConfig) -> Self {
        let bg = matting
            .options()
            .iter()
            .filter_map(|option| bg_sample_scale(&option.style))
            .reduce(f32::max);
        Self {
            cover: matting.fill_when_fits().is_some(),
            bg_sample_scale: bg,
        }
    }
}

/// Everything the loader needs to size a decode: the canvas the viewer renders
/// into plus what the configured mats ask of the source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecodeTarget {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub cover: bool,
    pub bg_sample_scale: Option<f32>,
}

/// Shared between the viewer (which knows the canvas) and the loader (which
/// decodes). Cloning shares the same slot.
#[derive(Clone, Debug, Default)]
pub struct DecodeScaling {
    needs: Option<DecodeNeeds>,
    canvas: Arc<Mutex<Option<(u32, u32)>>>,
}

impl DecodeScaling {
    /// Scaled decode off: every photo decodes at full size.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Derive the static requirements from `cfg`.
    ///
    /// Scaling is off when a photo effect is enabled: the effects work in
    /// source pixels (neighbour gradients, for instance), so running them on a
    /// smaller buffer would change the look, not just the cost.
    pub fn from_config(cfg: &Configuration) -> Self {
        if cfg.photo_effect.is_enabled() {
            return Self::disabled();
        }
        Self {
            needs: Some(DecodeNeeds::from_matting(&cfg.matting)),
            canvas: Arc::default(),
        }
    }

    /// Record the canvas the viewer currently renders into.
    pub fn publish_canvas(&self, canvas_w: u32, canvas_h: u32) {
        if self.needs.is_none() {
            return;
        }
        if let Ok(mut slot) = self.canvas.lock() {
            *slot = Some((canvas_w, canvas_h));
        }
    }

    /// The current decode target, or `None` while scaling is off or the canvas
    /// is not yet known.
    pub fn target(&self) -> Option<DecodeTarget> {
        let needs = self.needs?;
        let (canvas_w, canvas_h) = (*self.canvas.lock().ok()?)?;
        if canvas_w == 0 || canvas_h == 0 {
            return None;
        }
        Some(DecodeTarget {
            canvas_w,
            canvas_h,
            cover: needs.cover,
            bg_sample_scale: needs.bg_sample_scale,
        })
    }
}

/// True for the EXIF orientations that transpose the image (5..=8).
pub fn orientation_swaps_axes(orientation: u16) -> bool {
    matches!(orientation, 5..=8)
}

/// The smallest rect, in the displayed orientation, that every rendering of a
/// `disp_w` x `disp_h` photo draws into without upscaling.
pub fn required_size(disp_w: u32, disp_h: u32, target: &DecodeTarget) -> (u32, u32) {
    let sw = disp_w.max(1) as f64;
    let sh = disp_h.max(1) as f64;
    let cw = target.canvas_w.max(1) as f64;
    let ch = target.canvas_h.max(1) as f64;

    // The photo itself: contained in the canvas (mat margins only shrink it),
    // or cover-cropped to it for `fill-when-fits`.
    let mut scale = if target.cover {
        (cw / sw).max(ch / sh)
    } else {
        (cw / sw).min(ch / sh)
    };
    // A blurred background cover-crops the photo to the canvas aspect and then
    // shrinks it to the sample size, which needs the source to cover that.
    if let Some(k) = target.bg_sample_scale {
        let k = k as f64;
        scale = scale.max((cw * k / sw).max(ch * k / sh));
    }

    // Never ask for more than the source has.
    let scale = scale.min(1.0);
    let need = |len: f64| (len * scale - 1e-6).ceil().clamp(1.0, len) as u32;
    (need(sw), need(sh))
}

/// Output length of one axis at DCT scale `idct/8` as jpeg-decoder reports it
/// (rounded up: the last partial block is padded with replicated edge pixels).
pub fn dct_len(len: u16, idct: u8) -> u32 {
    (len as u32 * idct as u32).div_ceil(8)
}

/// Length of one axis of the image the loader hands on at DCT scale `idct/8`:
/// the exact scaled length `len * idct / 8`, rounded to the nearest pixel.
/// jpeg-decoder's padded output is resampled onto exactly this many pixels
/// spanning the photo's true extent, so the viewer's resample of the buffer
/// neither stretches nor shifts the frame.
pub fn scaled_len(len: u16, idct: u8) -> u32 {
    ((len as u32 * idct as u32 + 4) / 8).max(1)
}

/// The smallest DCT scale (`n/8`) whose output covers `need` on both axes, in
/// the file's own (unrotated) axes. `8` means decode in full.
pub fn choose_idct_size(file_w: u16, file_h: u16, need_w: u32, need_h: u32) -> u8 {
    IDCT_SIZES
        .into_iter()
        .find(|&n| scaled_len(file_w, n) >= need_w && scaled_len(file_h, n) >= need_h)
        .unwrap_or(8)
}

/// Whether `decoded` pixels (of a photo whose original size is `source`, both
/// in the displayed orientation) still cover everything the viewer will draw
/// from them into `canvas`: the contained photo, the cover fit when `cover`,
/// and a blurred background sampled at `bg_sample_scale`. Used by the matting
/// worker to catch photos decoded for a canvas that has since changed.
pub fn decoded_suffices(
    decoded: (u32, u32),
    source: (u32, u32),
    canvas: (u32, u32),
    cover: bool,
    bg_sample_scale: Option<f32>,
) -> bool {
    if decoded == source {
        return true;
    }
    let (need_w, need_h) = required_size(
        source.0,
        source.1,
        &DecodeTarget {
            canvas_w: canvas.0,
            canvas_h: canvas.1,
            cover,
            bg_sample_scale,
        },
    );
    decoded.0 >= need_w && decoded.1 >= need_h
}

/// Plan the scale for a JPEG of `file_w` x `file_h` stored pixels that EXIF
/// `orientation` will display rotated or not. Returns the DCT scale and the
/// required size in the file's axes.
pub fn plan(file_w: u16, file_h: u16, orientation: u16, target: &DecodeTarget) -> (u8, u32, u32) {
    let (disp_w, disp_h) = if orientation_swaps_axes(orientation) {
        (file_h as u32, file_w as u32)
    } else {
        (file_w as u32, file_h as u32)
    };
    let (need_dw, need_dh) = required_size(disp_w, disp_h, target);
    let (need_w, need_h) = if orientation_swaps_axes(orientation) {
        (need_dh, need_dw)
    } else {
        (need_dw, need_dh)
    };
    (
        choose_idct_size(file_w, file_h, need_w, need_h),
        need_w,
        need_h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MattingKind, MattingOptions};

    fn matting_of(kinds: &[MattingKind]) -> MattingConfig {
        MattingConfig::sequential_from(
            kinds
                .iter()
                .map(|&k| MattingOptions::default_for_showcase(k, 6.0, None).unwrap())
                .collect(),
        )
    }

    #[test]
    fn needs_follow_the_configured_mats() {
        let plain =
            DecodeNeeds::from_matting(&matting_of(&[MattingKind::Studio, MattingKind::FixedColor]));
        assert_eq!(
            plain,
            DecodeNeeds {
                cover: false,
                bg_sample_scale: None
            }
        );

        let blur =
            DecodeNeeds::from_matting(&matting_of(&[MattingKind::Studio, MattingKind::Blur]));
        assert_eq!(
            blur.bg_sample_scale,
            Some(MattingMode::default_blur_sample_scale())
        );
    }

    #[test]
    fn target_needs_published_canvas() {
        let scaling = DecodeScaling {
            needs: Some(DecodeNeeds {
                cover: false,
                bg_sample_scale: None,
            }),
            canvas: Arc::default(),
        };
        assert!(scaling.target().is_none());
        scaling.publish_canvas(3840, 2160);
        let clone = scaling.clone();
        assert_eq!(
            clone.target().map(|t| (t.canvas_w, t.canvas_h)),
            Some((3840, 2160))
        );
        scaling.publish_canvas(0, 0);
        assert!(scaling.target().is_none());
    }
}
