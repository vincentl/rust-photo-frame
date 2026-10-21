//! A photo as a set of windows (`local-markdown/p1-pairing-design.md` §5).
//!
//! Every mat kind places `n ≥ 1` windows inside the rectangle it already
//! computes for the photo. `n = 1` is the solo code path, bit-identical to
//! the matting worker before pairing (invariant C1, tested against goldens);
//! `n = 2` is a pair screen. The rules, by name:
//!
//! - **Logical shape.** Members are scaled to the smaller of their decoded
//!   heights `h₀` and set side by side with a gap of `g·h₀`, where
//!   `g = r·m/(1 − 2m)` (`r` = `pairing.bar-ratio`, `m` the mat's margin).
//!   The mat code fits that logical size with its existing margin and
//!   max-upscale logic; for pairable members the fit is height-bound (C2), so
//!   the drawn bar is `r × border` (`border = H_c·m`).
//! - **Windows.** At the fitted height `H`: a left window `(round(H·a_L), H)`,
//!   a bar of `round(g·H)`, a right window `(round(H·a_R), H)`, centred on the
//!   canvas. The aspects `a_i` are each photo's own (`source_size`), never its
//!   decoded buffer's, so a DCT-scaled decode lays out exactly as a full one.
//!   The bar is exact; the rounding of the three widths goes to the outer mat
//!   (at most a pixel a side). The first appearance goes on the left. **No
//!   cropping**: different aspects keep their full frame.
//! - **Drawing.** Each member is resized from its own decoded pixels straight
//!   to its window (C3: no resampling step a solo photo of the same mat kind
//!   does not have).
//! - **The bar shows the mat**: blurred backgrounds and swatches use the
//!   members **abutted with no gap**; studio bevels shade relative to the
//!   nearest window and are clamped to `bar/2`; the drop shadow is the union's.

use super::{ImgTex, MatResult, QueuedImage};
use crate::tasks::viewer::scenes::WakeScene;
use image::{RgbaImage, imageops};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use tracing::info;

/// A window: where one member is drawn on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Window {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The photos of one screen, in screen order (first appearance on the left),
/// as decoded, each with its own aspect (from `source_size`).
pub(super) struct PhotoSet {
    members: Vec<(RgbaImage, f32)>,
    /// `g`: the center bar as a fraction of photo height (unused for n = 1).
    bar_fraction: f32,
}

/// `g = r·m/(1 − 2m)`: the center bar as a fraction of photo height for bar
/// ratio `r` and mat margin `m`.
pub(super) fn bar_fraction(bar_ratio: f32, margin: f32) -> f32 {
    bar_ratio * margin / (1.0 - 2.0 * margin)
}

/// A photo's own aspect, `w/h` of its displayed size.
pub(super) fn source_aspect((w, h): (u32, u32)) -> f32 {
    w.max(1) as f32 / h.max(1) as f32
}

impl PhotoSet {
    pub(super) fn solo(photo: RgbaImage) -> Self {
        let aspect = source_aspect(photo.dimensions());
        Self {
            members: vec![(photo, aspect)],
            bar_fraction: 0.0,
        }
    }

    /// `left` and `right` as decoded, with their photos' displayed sizes.
    pub(super) fn pair(
        (left, left_size): (RgbaImage, (u32, u32)),
        (right, right_size): (RgbaImage, (u32, u32)),
        bar_fraction: f32,
    ) -> Self {
        Self {
            members: vec![
                (left, source_aspect(left_size)),
                (right, source_aspect(right_size)),
            ],
            bar_fraction,
        }
    }

    pub(super) fn is_pair(&self) -> bool {
        self.members.len() > 1
    }

    /// `h₀`, the smaller decoded height.
    fn common_height(&self) -> u32 {
        self.members
            .iter()
            .map(|(img, _)| img.height())
            .min()
            .unwrap_or(1)
            .max(1)
    }

    /// **Logical shape**: the size the mat code fits. n = 1: the photo's
    /// decoded size (the solo path); n = 2: the members at `h₀`, side by side
    /// with a gap of `g·h₀`.
    pub(super) fn logical_size(&self) -> (u32, u32) {
        if !self.is_pair() {
            return self.members[0].0.dimensions();
        }
        let h0 = self.common_height();
        let aspects: f32 = self.members.iter().map(|(_, a)| a).sum();
        let w = (h0 as f32 * (aspects + self.bar_fraction)).round().max(1.0);
        (w as u32, h0)
    }

    /// **Windows** for the fitted rect: n = 1, the rect itself; n = 2, the
    /// members at the rect's height `H` with a bar of `round(g·H)` between
    /// them, centred on a canvas `canvas_w` wide.
    pub(super) fn windows(&self, rect: Window, canvas_w: u32) -> Vec<Window> {
        if !self.is_pair() {
            return vec![rect];
        }
        let h = rect.h;
        let width = |aspect: f32| ((h as f32 * aspect).round() as u32).max(1);
        let (left_w, right_w) = (width(self.members[0].1), width(self.members[1].1));
        let bar = (h as f32 * self.bar_fraction).round() as u32;
        let x = canvas_w.saturating_sub(left_w + bar + right_w) / 2;
        vec![
            Window {
                x,
                y: rect.y,
                w: left_w,
                h,
            },
            Window {
                x: x + left_w + bar,
                y: rect.y,
                w: right_w,
                h,
            },
        ]
    }

    /// **Drawing**: each member resized from its own decoded pixels straight
    /// to its window (borrowed when it already has that size, as the solo
    /// path does).
    pub(super) fn resize_to(&self, windows: &[Window]) -> Vec<Cow<'_, RgbaImage>> {
        self.members
            .iter()
            .zip(windows)
            .map(|((img, _), win)| {
                if img.dimensions() == (win.w, win.h) {
                    Cow::Borrowed(img)
                } else {
                    Cow::Owned(imageops::resize(
                        img,
                        win.w,
                        win.h,
                        imageops::FilterType::Triangle,
                    ))
                }
            })
            .collect()
    }

    /// The members **abutted with no gap** at `h₀` (n = 1: the photo itself),
    /// for swatches and blurred backgrounds, so the bar shows the mat rather
    /// than a fringe or a stripe.
    pub(super) fn abutted(&self) -> Cow<'_, RgbaImage> {
        if !self.is_pair() {
            return Cow::Borrowed(&self.members[0].0);
        }
        let h0 = self.common_height();
        let scaled: Vec<Cow<'_, RgbaImage>> = self
            .members
            .iter()
            .map(|(img, aspect)| {
                let w = ((h0 as f32 * aspect).round() as u32).max(1);
                if img.dimensions() == (w, h0) {
                    Cow::Borrowed(img)
                } else {
                    Cow::Owned(imageops::resize(img, w, h0, imageops::FilterType::Triangle))
                }
            })
            .collect();
        let total_w = scaled.iter().map(|img| img.width()).sum();
        let mut out = RgbaImage::new(total_w, h0);
        let mut x = 0;
        for img in &scaled {
            imageops::replace(&mut out, img.as_ref(), i64::from(x), 0);
            x += img.width();
        }
        Cow::Owned(out)
    }
}

/// Windows paired with the pixels drawn into them.
pub(super) fn placed<'a>(
    windows: &[Window],
    images: &'a [Cow<'a, RgbaImage>],
) -> Vec<(Window, &'a RgbaImage)> {
    windows
        .iter()
        .copied()
        .zip(images.iter().map(AsRef::as_ref))
        .collect()
}

/// Overlay kinds: draw each member at its window over the background.
pub(super) fn overlay_windows(background: &mut RgbaImage, placed: &[(Window, &RgbaImage)]) {
    for (win, img) in placed {
        imageops::overlay(background, *img, i64::from(win.x), i64::from(win.y));
    }
}

/// One member of a screen as the viewer tracks it: path and priority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Member {
    pub path: PathBuf,
    pub priority: bool,
}

// ── Membership: per-member priority (B2) and purge (R14) ───────────────────

impl QueuedImage {
    /// Paths of the members that carry priority (B2: per member).
    pub(super) fn priority_paths(&self) -> Vec<PathBuf> {
        let first = self.priority.then(|| self.image.path.clone());
        let second = self
            .partner
            .as_ref()
            .filter(|p| p.priority)
            .map(|p| p.prepared.path.clone());
        first.into_iter().chain(second).collect()
    }

    fn contains(&self, path: &Path) -> bool {
        self.image.path == path
            || self
                .partner
                .as_ref()
                .is_some_and(|p| p.prepared.path == path)
    }

    /// Before matting, a pair containing `path` degrades to its other member;
    /// a solo `path` is dropped (`None`).
    fn without(self, path: &Path) -> Option<Self> {
        let QueuedImage {
            image,
            priority,
            partner,
        } = self;
        match partner {
            Some(partner) if image.path == path => Some(QueuedImage {
                image: partner.prepared,
                priority: partner.priority,
                partner: None,
            }),
            Some(partner) if partner.prepared.path == path => Some(QueuedImage {
                image,
                priority,
                partner: None,
            }),
            None if image.path == path => None,
            partner => Some(QueuedImage {
                image,
                priority,
                partner,
            }),
        }
    }
}

/// A finished canvas; a pair's layout is logged (`pair_canvas`, debug).
pub(super) fn matted(result: MatResult) -> MatResult {
    if let Some(partner) = &result.partner {
        tracing::debug!(
            path = %result.path.display(),
            partner = %partner.path.display(),
            windows = ?result.windows,
            "pair_canvas"
        );
    }
    result
}

impl MatResult {
    /// A screen is priority iff any member is (B2).
    pub(super) fn is_priority(&self) -> bool {
        self.priority || self.partner.as_ref().is_some_and(|p| p.priority)
    }

    pub(super) fn priority_paths(&self) -> Vec<PathBuf> {
        let first = self.priority.then(|| self.path.clone());
        let second = self
            .partner
            .as_ref()
            .filter(|p| p.priority)
            .map(|p| p.path.clone());
        first.into_iter().chain(second).collect()
    }

    pub(super) fn contains(&self, path: &Path) -> bool {
        self.path == path || self.partner.as_ref().is_some_and(|p| p.path == path)
    }
}

impl ImgTex {
    pub(super) fn contains(&self, path: &Path) -> bool {
        self.path == path || self.partner.as_deref() == Some(path)
    }

    /// What the manager is told when this screen is shown.
    pub(super) fn displayed(&self) -> crate::events::Displayed {
        crate::events::Displayed {
            path: self.path.clone(),
            partner: self.partner.clone(),
        }
    }
}

fn log_pair_purged(path: &Path, stage: &'static str) {
    info!(path = %path.display(), stage, "pair_purged");
}

/// **Bridge priority purge (R14), by membership.** A priority photo `path`
/// is about to be matted fresh; drop its stale copies. Before matting, a
/// queued pair containing it degrades to the other member. After
/// compositing (a matted canvas, an uploaded texture, the staged next
/// screen), the canvas is dropped and `pair_purged` is logged, the same loss
/// today's purge imposes on `path` itself.
pub(super) fn purge_member(
    path: &Path,
    deferred: &mut VecDeque<QueuedImage>,
    ready: &mut VecDeque<MatResult>,
    wake: &mut WakeScene,
) {
    *deferred = std::mem::take(deferred)
        .into_iter()
        .filter_map(|queued| {
            if queued.contains(path) {
                queued.without(path)
            } else {
                Some(queued)
            }
        })
        .collect();
    purge_results(path, ready);
    wake.pending_mut().retain(|tex| {
        let keep = !tex.contains(path);
        if !keep && tex.partner.is_some() {
            log_pair_purged(path, "pending");
        }
        keep
    });
    if wake.next().is_some_and(|stage| stage.contains(path)) {
        if wake.next().is_some_and(|stage| stage.partner.is_some()) {
            log_pair_purged(path, "next");
        }
        wake.set_next(None);
    }
}

/// Drop matted canvases containing `path` (a pair's canvas is lost whole).
fn purge_results(path: &Path, ready: &mut VecDeque<MatResult>) {
    ready.retain(|result| {
        let keep = !result.contains(path);
        if !keep && result.partner.is_some() {
            log_pair_purged(path, "matted");
        }
        keep
    });
}

/// Queue a finished canvas; its priority members purge older canvases that
/// contain them (`drain_mat_results` and the KMS loop's `accept_mat_result`).
pub(super) fn queue_result(ready: &mut VecDeque<MatResult>, result: MatResult) {
    for path in result.priority_paths() {
        purge_results(&path, ready);
    }
    ready.push_back(result);
}

/// Upload of a priority screen: older uploaded screens sharing a priority
/// member are dropped (a pair's canvas whole, logged `pair_purged`), a staged
/// next screen sharing one goes back to the queue, and the new screen jumps
/// to the front.
pub(super) fn stage_priority(wake: &mut WakeScene, tex: ImgTex, priority_paths: &[PathBuf]) {
    let shares_member = |queued: &ImgTex| priority_paths.iter().any(|path| queued.contains(path));
    let replace_next = wake.next().is_some_and(shares_member);
    wake.pending_mut().retain(|queued| {
        let keep = !shares_member(queued);
        if !keep && queued.partner.is_some() {
            log_pair_purged(&queued.path, "uploaded");
        }
        keep
    });
    if replace_next && let Some(stage) = wake.take_next() {
        wake.pending_mut().push_front(stage);
    }
    wake.pending_mut().push_front(tex);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::viewer::ImagePlane;

    fn result(path: &str, priority: bool, partner: Option<(&str, bool)>) -> MatResult {
        MatResult {
            path: PathBuf::from(path),
            canvas: ImagePlane {
                width: 1,
                height: 1,
                pixels: vec![0; 4],
            },
            priority,
            partner: partner.map(|(path, priority)| Member {
                path: PathBuf::from(path),
                priority,
            }),
            windows: Vec::new(),
            mat_kind: None,
        }
    }

    fn paths(ready: &VecDeque<MatResult>) -> Vec<String> {
        ready
            .iter()
            .map(|r| match &r.partner {
                Some(p) => format!("{}+{}", r.path.display(), p.path.display()),
                None => r.path.display().to_string(),
            })
            .collect()
    }

    #[test]
    fn a_screen_is_priority_iff_any_member_is() {
        assert!(!result("a", false, Some(("b", false))).is_priority());
        assert!(result("a", false, Some(("b", true))).is_priority());
        assert!(result("a", true, None).is_priority());
        assert_eq!(
            result("a", false, Some(("b", true))).priority_paths(),
            [PathBuf::from("b")]
        );
    }

    #[test]
    fn a_priority_member_purges_older_canvases_containing_it() {
        let mut ready = VecDeque::new();
        queue_result(&mut ready, result("x", false, Some(("y", false))));
        queue_result(&mut ready, result("z", false, None));
        queue_result(&mut ready, result("y", false, None));
        // A debut second in a pair purges both older canvases with it.
        queue_result(&mut ready, result("w", false, Some(("y", true))));
        assert_eq!(paths(&ready), ["z", "w+y"]);
    }

    #[test]
    fn windows_put_an_exact_bar_between_full_height_members() {
        let set = PhotoSet::pair(
            (RgbaImage::new(30, 40), (3000, 4000)),
            (RgbaImage::new(24, 40), (2400, 4000)),
            bar_fraction(0.618, 0.06),
        );
        let rect = Window {
            x: 0,
            y: 65,
            w: 0,
            h: 1901,
        };
        let w = set.windows(rect, 3840);
        assert_eq!((w[0].h, w[1].h), (1901, 1901));
        assert_eq!(w[0].w, (1901.0f32 * 0.75).round() as u32);
        assert_eq!(w[1].w, (1901.0f32 * 0.6).round() as u32);
        let bar = w[1].x - (w[0].x + w[0].w);
        assert_eq!(bar, (1901.0 * bar_fraction(0.618, 0.06)).round() as u32);
        let right_margin = 3840 - (w[1].x + w[1].w);
        assert!(w[0].x.abs_diff(right_margin) <= 1, "centred");
        // Solo: the fitted rect itself.
        let solo = PhotoSet::solo(RgbaImage::new(30, 40));
        assert_eq!(solo.windows(rect, 3840), [rect]);
    }
}
