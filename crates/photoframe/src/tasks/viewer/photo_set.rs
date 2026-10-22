//! A photo as a set of windows (`local-markdown/p1-pairing-design.md` §5).
//!
//! Every mat kind draws `n ≥ 1` windows in the photo space it already
//! computes. `n = 1` is the solo code path, bit-identical to the matting
//! worker before pairing (invariant C1, tested against goldens); `n = 2` is a
//! pair screen, laid out explicitly in that space. The rules, by name:
//!
//! - **Height.** With `a_L`, `a_R` the members' aspects, `b` the mat's side
//!   margin (`minimum-mat-percentage` of the canvas width, as the solo fit
//!   reserves it), `r` = `pairing.bar-ratio` and `β` the bevel a studio or
//!   passe-partout mat cuts around each window (0 elsewhere), the pair's
//!   height is the solo fit's
//!   `H = min(avail_h, (W_c − (2 + r)·b − 4β) / (a_L + a_R), u·h₀)`: the
//!   mat's own available height (so a pairable member keeps its solo height,
//!   C2), the width that leaves each side margin at least `b`, and the
//!   max-upscale cap on the smaller decoded height `h₀`, exactly as the solo
//!   fit applies them.
//! - **Windows.** A left window `(round(H·a_L), H)` and a right window
//!   `(round(H·a_R), H)`, centred vertically. The horizontal slack
//!   `S = W_c − w_L − w_R − 4β` is shared between the left margin, the bar and
//!   the right margin with weights `1 : r : 1`: the bar is `round(r·S/(2 + r))`
//!   (plus `⌈2β⌉` for the two inner bevels), the rounding of the widths goes
//!   to the outer mat (so a margin can fall up to a pixel short of `b` at the
//!   width bound), and `r = 0` puts the photos edge to edge. The aspects
//!   `a_i` are each photo's own (`source_size`),
//!   never its decoded buffer's, so a DCT-scaled decode lays out exactly as a
//!   full one. The first appearance goes on the left. **No cropping**:
//!   different aspects keep their full frame.
//! - **Drawing.** Each member is resized from its own decoded pixels straight
//!   to its window (C3: no resampling step a solo photo of the same mat kind
//!   does not have).
//! - **The bar shows the mat**: blurred backgrounds and swatches use the
//!   members **abutted with no gap**; studio bevels shade relative to the
//!   nearest window and are clamped to `bar/2`; the drop shadow is the union's.

use super::{ImgTex, MatResult, QueuedImage};
use crate::tasks::manager::pairing::PairingRules;
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
    /// How a pair shares the mat (unused for n = 1).
    spacing: PairSpacing,
}

/// A pair's share of the mat: `r` = `pairing.bar-ratio`, the bar's weight
/// against each side margin, and `b`, the mat's side margin in canvas pixels
/// (`minimum-mat-percentage` of the canvas width, what the solo fit reserves
/// on each side), the floor under each side margin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PairSpacing {
    pub bar_ratio: f32,
    pub border: f32,
}

impl PairSpacing {
    /// `r` and `b` for `bar_ratio` on a `canvas` matted with `margin`
    /// (`minimum-mat-percentage / 100`).
    pub(super) fn new(bar_ratio: f32, margin: f32, canvas: (u32, u32)) -> Self {
        Self {
            bar_ratio: bar_ratio.max(0.0),
            border: margin.max(0.0) * canvas.0 as f32,
        }
    }
}

/// The space a mat offers a pair: the canvas, the height the solo fit would
/// allow (the canvas less the mat's vertical margins and bevels, clamped as
/// that mat clamps it), the bevel it cuts around each window (0 for overlay
/// kinds) and the max-upscale factor.
#[derive(Clone, Copy, Debug)]
pub(super) struct PhotoSpace {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub avail_h: f32,
    pub bevel_px: f32,
    pub max_upscale: f32,
}

impl PhotoSpace {
    /// The two inner bevels' share of the bar, in pixels, rounded up so each
    /// bevel fits whole (the renderer clamps a bevel to half the bar).
    fn bar_bevels(&self) -> u32 {
        (2.0 * self.bevel_px.max(0.0)).ceil() as u32
    }
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
            spacing: PairSpacing {
                bar_ratio: 0.0,
                border: 0.0,
            },
        }
    }

    /// `left` and `right` as decoded, with their photos' displayed sizes.
    pub(super) fn pair(
        (left, left_size): (RgbaImage, (u32, u32)),
        (right, right_size): (RgbaImage, (u32, u32)),
        spacing: PairSpacing,
    ) -> Self {
        Self {
            members: vec![
                (left, source_aspect(left_size)),
                (right, source_aspect(right_size)),
            ],
            spacing,
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

    /// n = 1: the photo's decoded size, which the solo path fits as before.
    pub(super) fn solo_size(&self) -> (u32, u32) {
        debug_assert!(!self.is_pair());
        self.members[0].0.dimensions()
    }

    /// n = 2: **Height** and **Windows** in `space` (the module doc). The
    /// height is the solo fit's, `round(h₀·s)` for the scale
    /// `s = min(avail_h/h₀, avail_w/((a_L + a_R)·h₀), u)` with
    /// `avail_w = W_c − (2 + r)·b − 4β`, so a height-bound pair is exactly as
    /// tall as its members alone on this mat. The slack left after the two
    /// windows and the four bevels is shared `1 : r : 1` between the left
    /// margin, the bar and the right margin.
    pub(super) fn pair_windows(&self, space: PhotoSpace) -> Vec<Window> {
        debug_assert!(self.is_pair());
        let (a_left, a_right) = (self.members[0].1, self.members[1].1);
        let aspects = (a_left + a_right).max(f32::EPSILON);
        let h0 = self.common_height() as f32;
        let PairSpacing {
            bar_ratio: r,
            border,
        } = self.spacing;
        let bar_bevels = space.bar_bevels();
        let bevels = 2.0 * space.bevel_px.max(0.0) + bar_bevels as f32;
        let avail_w = (space.canvas_w as f32 - (2.0 + r) * border - bevels).max(1.0);
        let avail_h = space.avail_h.max(1.0);
        let mut scale = (avail_h / h0)
            .min(avail_w / (aspects * h0))
            .min(space.max_upscale.max(1.0));
        if !scale.is_finite() || scale <= 0.0 {
            scale = 1.0;
        }
        let h = ((h0 * scale).round() as u32).clamp(1, space.canvas_h.max(1));
        let width = |aspect: f32| ((h as f32 * aspect).round() as u32).max(1);
        let (left_w, right_w) = (width(a_left), width(a_right));
        let slack = (space.canvas_w as f32 - (left_w + right_w) as f32 - bevels).max(0.0);
        let bar = (r * slack / (2.0 + r)).round() as u32 + bar_bevels;
        let x = space.canvas_w.saturating_sub(left_w + bar + right_w) / 2;
        let y = space.canvas_h.saturating_sub(h) / 2;
        vec![
            Window { x, y, w: left_w, h },
            Window {
                x: x + left_w + bar,
                y,
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
    /// Decoded photos this screen holds: 1, or 2 for a pair.
    pub(super) fn photos(&self) -> usize {
        1 + usize::from(self.partner.is_some())
    }

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

    /// Whether this pair may be drawn on `canvas`: both members pairable
    /// there (R2), which is what guarantees C2. `rules` is `None` with
    /// pairing off.
    pub(super) fn pair_fits(&self, rules: Option<&PairingRules>, canvas: (u32, u32)) -> bool {
        let Some(partner) = &self.partner else {
            return true;
        };
        rules.is_some_and(|rules| {
            rules.is_pairable(Some(self.image.source_size), Some(canvas))
                && rules.is_pairable(Some(partner.prepared.source_size), Some(canvas))
        })
    }

    /// A pair as two solo screens, in order, each member with its priority.
    pub(super) fn split(self) -> (QueuedImage, QueuedImage) {
        let partner = self.partner.expect("split a pair");
        (
            QueuedImage {
                image: self.image,
                priority: self.priority,
                partner: None,
            },
            QueuedImage {
                image: partner.prepared,
                priority: partner.priority,
                partner: None,
            },
        )
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

impl super::MatTask {
    /// Decoded photos the task carries: 1, or 2 for a pair.
    pub(super) fn photos(&self) -> usize {
        1 + usize::from(self.partner.is_some())
    }
}

impl MatResult {
    /// The worker's answer to a task that produced no canvas (an unreadable
    /// or zero-size image): it carries only the task's photo count, which the
    /// viewer gives back to `mat_inflight` before dropping it. A real result
    /// always has a canvas of at least 1 x 1.
    pub(super) fn acknowledgement(path: PathBuf, photos: usize) -> MatResult {
        MatResult {
            path,
            canvas: super::ImagePlane {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
            priority: false,
            partner: None,
            windows: Vec::new(),
            mat_kind: None,
            photos,
        }
    }

    pub(super) fn is_acknowledgement(&self) -> bool {
        self.canvas.width == 0
    }

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
            photos: 1,
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

    /// A pair of `left` and `right` (displayed sizes) decoded at those sizes.
    fn pair(left: (u32, u32), right: (u32, u32), bar_ratio: f32, margin: f32) -> PhotoSet {
        PhotoSet::pair(
            (RgbaImage::new(left.0, left.1), left),
            (RgbaImage::new(right.0, right.1), right),
            PairSpacing::new(bar_ratio, margin, (3840, 2160)),
        )
    }

    /// A 6% mat on 3840x2160 that stands its photos 1900 px tall (two
    /// 130 px borders), with no bevel.
    fn uhd_6pct() -> PhotoSpace {
        PhotoSpace {
            canvas_w: 3840,
            canvas_h: 2160,
            avail_h: 1900.0,
            bevel_px: 0.0,
            max_upscale: 1.0,
        }
    }

    /// `(left margin, left width, bar, right width, right margin)` and the
    /// common height of a pair's windows on a canvas `canvas_w` wide.
    fn layout(w: &[Window], canvas_w: u32) -> ((u32, u32, u32, u32, u32), u32) {
        assert_eq!(w[0].h, w[1].h);
        assert_eq!(w[0].y, w[1].y);
        (
            (
                w[0].x,
                w[0].w,
                w[1].x - (w[0].x + w[0].w),
                w[1].w,
                canvas_w - (w[1].x + w[1].w),
            ),
            w[0].h,
        )
    }

    /// Golden numbers on 3840x2160, 6%: two 3:4 portraits at `r = 0.618` are
    /// 1900 tall and 1425 wide; the slack of 990 is shared 1 : 0.618 : 1 as
    /// margins of 378 and a bar of 234.
    #[test]
    fn two_3_4_portraits_at_the_default_ratio() {
        let set = pair((1500, 2000), (1500, 2000), 0.618, 0.06);
        let w = set.pair_windows(uhd_6pct());
        assert_eq!(layout(&w, 3840), ((378, 1425, 234, 1425, 378), 1900));
        assert_eq!(w[0].y, 130, "centred vertically: (2160 − 1900) / 2");
    }

    /// A 3:4 beside a 4:5: windows 1425 and 1520, slack 895, margins 342
    /// (341.9), bar 211 (211.3).
    #[test]
    fn a_3_4_beside_a_4_5() {
        let set = pair((1500, 2000), (1600, 2000), 0.618, 0.06);
        let w = set.pair_windows(uhd_6pct());
        assert_eq!(layout(&w, 3840), ((342, 1425, 211, 1520, 342), 1900));
    }

    /// `r = 0`: edge to edge, all the slack to the sides.
    #[test]
    fn ratio_zero_puts_the_photos_edge_to_edge() {
        let set = pair((1500, 2000), (1500, 2000), 0.0, 0.06);
        let w = set.pair_windows(uhd_6pct());
        assert_eq!(layout(&w, 3840), ((495, 1425, 0, 1425, 495), 1900));
    }

    /// Width-bound: two squares on a 4:3 canvas (1600x1200, 6%, side margin
    /// 96) cannot stand 1056 tall; the width `1600 − 2.618 × 96 = 1348.7`
    /// leaves them 674 each, the side margins the mat's own (the odd pixel to
    /// the right) and the bar `round(0.618 × 252 / 2.618) = 59`, centred in
    /// the leftover height.
    #[test]
    fn a_width_bound_pair_keeps_its_margins_at_the_border() {
        let set = PhotoSet::pair(
            (RgbaImage::new(2000, 2000), (2000, 2000)),
            (RgbaImage::new(2000, 2000), (2000, 2000)),
            PairSpacing::new(0.618, 0.06, (1600, 1200)),
        );
        let w = set.pair_windows(PhotoSpace {
            canvas_w: 1600,
            canvas_h: 1200,
            avail_h: 1056.0,
            bevel_px: 0.0,
            max_upscale: 1.0,
        });
        assert_eq!(layout(&w, 1600), ((96, 674, 59, 674, 97), 674));
        assert_eq!(w[0].y, 263, "(1200 − 674) / 2");
    }

    /// A 2% fixed-colour mat: the overlay fit stands the photos
    /// `round(2160 × 0.96) = 2074` tall (decodes of 2200 px, so the upscale
    /// cap is clear), so 1556 wide (1555.5 rounds up), slack 728, margins
    /// 278, bar 172 (171.9).
    #[test]
    fn a_2_percent_mat() {
        let set = pair((1650, 2200), (1650, 2200), 0.618, 0.02);
        let w = set.pair_windows(PhotoSpace {
            avail_h: 2160.0 * 0.96,
            ..uhd_6pct()
        });
        assert_eq!(layout(&w, 3840), ((278, 1556, 172, 1556, 278), 2074));
    }

    /// Studio, 6%, a 4 px bevel: the photo space is 1892 tall; the four
    /// bevels take 16 of the width, the flat boards share the rest 1 : r : 1
    /// (376.6 : 232.8 : 376.6), and the bar holds the two inner bevels.
    #[test]
    fn bevels_sit_in_the_bar_and_the_margins() {
        let set = pair((1500, 2000), (1500, 2000), 0.618, 0.06);
        let w = set.pair_windows(PhotoSpace {
            avail_h: 1892.0,
            bevel_px: 4.0,
            ..uhd_6pct()
        });
        assert_eq!(layout(&w, 3840), ((380, 1419, 241, 1419, 381), 1892));
    }

    /// The solo fit's max-upscale cap: the height never exceeds `u × h₀`,
    /// the smaller decoded height.
    #[test]
    fn the_height_is_capped_by_max_upscale_of_the_smaller_decode() {
        let set = PhotoSet::pair(
            (RgbaImage::new(600, 800), (1500, 2000)),
            (RgbaImage::new(1500, 2000), (1500, 2000)),
            PairSpacing::new(0.618, 0.06, (3840, 2160)),
        );
        for (u, h) in [(1.0, 800), (2.0, 1600), (3.0, 1900)] {
            let w = set.pair_windows(PhotoSpace {
                max_upscale: u,
                ..uhd_6pct()
            });
            assert_eq!(w[0].h, h, "u = {u}");
            assert_eq!(w[0].w, (h as f32 * 0.75).round() as u32);
        }
    }
}
