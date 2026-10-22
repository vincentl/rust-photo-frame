//! Portrait pairing: what the manager knows about each photo's shape, and the
//! rule that decides whether a photo may share a screen.
//!
//! See `local-markdown/p1-pairing-design.md` §1. The names here follow it:
//! the **meta authority** (R10) and the **pairable** rule (R2, with the
//! no-shrink invariant C2).

use crate::config::{Configuration, MattingConfig, PairingOptions};
use crate::events::{CanvasShape, MetaSource, PhotoInfo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Displayed size of every photo in the inventory, under one authority
/// (design R10):
/// - `PhotoAdded` for a new path records its inline read (possibly `None`);
///   for a known path (rename, replace) it overwrites only when that read is
///   `Some`.
/// - `PhotoMeta { Header }` (startup pass) **fills only**: it applies only
///   while the size is `None`, so a stale pass result loses to a fresh
///   `PhotoAdded`.
/// - `PhotoMeta { Decoded }` (the loader, after every decode) **always
///   overwrites**: a file caught mid-copy becomes pairable after its first
///   solo show, and an in-place rotation is corrected at its next decode.
/// - `PhotoMeta` applies only to known paths, so it cannot resurrect a
///   removed photo.
#[derive(Debug, Default)]
pub struct DisplaySizes {
    sizes: HashMap<PathBuf, Option<(u32, u32)>>,
}

impl DisplaySizes {
    pub fn on_added(&mut self, info: &PhotoInfo) {
        match self.sizes.get_mut(&info.path) {
            Some(slot) => {
                if info.display_size.is_some() {
                    *slot = info.display_size;
                }
            }
            None => {
                self.sizes.insert(info.path.clone(), info.display_size);
            }
        }
    }

    pub fn on_removed(&mut self, path: &Path) {
        self.sizes.remove(path);
    }

    /// Apply a `PhotoMeta`; returns whether it changed the recorded size.
    pub fn on_meta(&mut self, path: &Path, display_size: (u32, u32), source: MetaSource) -> bool {
        let Some(slot) = self.sizes.get_mut(path) else {
            return false; // unknown path: never resurrect a removed photo
        };
        let apply = match source {
            MetaSource::Header => slot.is_none(),
            MetaSource::Decoded => *slot != Some(display_size),
        };
        if apply {
            *slot = Some(display_size);
        }
        apply
    }

    /// The recorded displayed size, `None` when unknown or not in inventory.
    pub fn get(&self, path: &Path) -> Option<(u32, u32)> {
        self.sizes.get(path).copied().flatten()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, Option<(u32, u32)>)> {
        self.sizes.iter().map(|(path, size)| (path, *size))
    }
}

/// The static half of the pairable rule, fixed by configuration. Notation
/// as in the design (§1):
/// - `u` = `max-upscale-factor` (at least 1, as the matting worker applies it);
/// - `m_min`, `m_max` = smallest and largest margin fraction **among the
///   non-zero mat options** (the only mats a pair screen can get, §5);
/// - `r` = `pairing.bar-ratio`, the bar's weight against each side margin:
///   the horizontal slack after the two windows is shared `1 : r : 1`, and
///   each side margin is at least the mat border `b = m·H_c` (the shorter
///   side's share), see `viewer/photo_set.rs`;
/// - `b_max` = the widest bevel among the non-zero mats (studio and
///   passe-partout cut one around each window; the bar holds both inner
///   bevels, the margins the outer ones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairingRules {
    /// `pairing.probability`, the pairing coin's `p`.
    pub probability: f64,
    /// `r`, `pairing.bar-ratio`.
    pub bar_ratio: f64,
    /// `u`.
    pub max_upscale: f64,
    /// `m_min`.
    pub m_min: f64,
    /// `m_max`.
    pub m_max: f64,
    /// `b_max`, in canvas pixels.
    pub bevel_max: f64,
    /// `W`, the look-ahead window: unsent playlist positions after the head a
    /// partner may come from (design "Look-ahead pairing"). 1 is back to back
    /// only (rc2); `pairing.lookahead` sets it to `viewer-preload-count`
    /// capped at [`LOOKAHEAD_MAX`].
    pub lookahead: usize,
}

/// The widest look-ahead window, the default `viewer-preload-count`. The
/// window follows a shallower preload down (W = preload) but never exceeds
/// this: a deeper preload is a decode buffer, not a licence to move photos
/// further from their playlist slot (review C6).
pub const LOOKAHEAD_MAX: usize = 3;

impl PairingRules {
    /// `None` when pairing is disabled (or, impossible after validation, no
    /// non-zero mat is configured).
    pub fn from_config(cfg: &Configuration) -> Option<Self> {
        let pairing = &cfg.playlist.pairing;
        if !pairing.enabled {
            return None;
        }
        let rules = Self::from_parts(
            pairing,
            &cfg.matting,
            cfg.global_photo_settings.max_upscale_factor,
        )?;
        Some(Self {
            lookahead: if pairing.lookahead {
                cfg.viewer_preload_count.clamp(1, LOOKAHEAD_MAX)
            } else {
                1
            },
            ..rules
        })
    }

    /// The rules for `pairing` with these mats and max-upscale factor (the
    /// viewer's check before drawing a pair, and test harnesses). `None` when
    /// no non-zero mat is configured. The look-ahead window is 1 here (it
    /// needs `viewer-preload-count`; see [`Self::from_config`]).
    pub fn from_parts(
        pairing: &PairingOptions,
        matting: &MattingConfig,
        max_upscale_factor: f32,
    ) -> Option<Self> {
        let (m_min, m_max) = matting.nonzero_margin_range()?;
        Some(Self {
            probability: pairing.probability,
            bar_ratio: f64::from(pairing.bar_ratio),
            max_upscale: f64::from(max_upscale_factor.max(1.0)),
            m_min: f64::from(m_min),
            m_max: f64::from(m_max),
            bevel_max: f64::from(matting.nonzero_bevel_max()),
            lookahead: 1,
        })
    }

    /// **Shape limit.** The largest aspect a pairable photo may have on
    /// `canvas` (`A = W_c/H_c`): for a mat of margin `m` two photos at the
    /// solo height `D = H_c(1 − 2m)` stay height-bound iff
    /// `(a_P + a_Q)·D ≤ W_c − (2 + r)·m·H_c − 4·b_max` (the width that leaves
    /// each side margin its border and room for the four bevels), so
    /// `a_i ≤ (A − (2 + r)·m − 4·b_max/H_c) / (2(1 − 2m))` for both suffices.
    /// That bound is monotone in `m` (its derivative has the sign of
    /// `2A − (2 + r)`), so the tightest of the mats a pair can get is at
    /// `m_min` or `m_max`; the limit is the smaller of the two. Bevelled mats
    /// stand their photos `2·b_max` shorter, which only loosens the width
    /// bound; the rounding of their margins and bevels can cost at most a
    /// pixel at the boundary.
    pub fn shape_limit(&self, canvas: (u32, u32)) -> f64 {
        let (canvas_w, canvas_h) = (f64::from(canvas.0.max(1)), f64::from(canvas.1.max(1)));
        let canvas_aspect = canvas_w / canvas_h;
        let bevel_term = 4.0 * self.bevel_max / canvas_h;
        let limit = |m: f64| {
            (canvas_aspect - (2.0 + self.bar_ratio) * m - bevel_term) / (2.0 * (1.0 - 2.0 * m))
        };
        limit(self.m_min).min(limit(self.m_max))
    }

    /// **Pairable (R2).** Photo `i` (displayed size `w_i x h_i`, aspect
    /// `a_i = w_i/h_i`) is pairable iff
    /// - the canvas `(W_c, H_c)` has been published,
    /// - its size is known,
    /// - **shape condition:** `a_i ≤` [`Self::shape_limit`], and
    /// - **full-height condition:** `h_i·u ≥ H_c(1 − 2·m_min)`.
    ///
    /// Together these give the no-shrink invariant C2: each member of a pair
    /// is drawn at exactly the height it would have alone, for every mat a
    /// pair can get (proof in the design, §1).
    pub fn is_pairable(
        &self,
        display_size: Option<(u32, u32)>,
        canvas: Option<(u32, u32)>,
    ) -> bool {
        let (Some((w, h)), Some((canvas_w, canvas_h))) = (display_size, canvas) else {
            return false;
        };
        if w == 0 || h == 0 || canvas_w == 0 || canvas_h == 0 {
            return false;
        }
        let a = f64::from(w) / f64::from(h);
        let shape = a <= self.shape_limit((canvas_w, canvas_h));
        let full_height =
            f64::from(h) * self.max_upscale >= f64::from(canvas_h) * (1.0 - 2.0 * self.m_min);
        shape && full_height
    }
}

/// Portrait pairing's inputs to the live manager loop (`None` there means
/// pairing is off).
#[derive(Debug, Clone)]
pub struct LivePairing {
    pub rules: PairingRules,
    /// Published by the viewer; read at each decision.
    pub canvas: CanvasShape,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::CreatedSource;
    use std::time::SystemTime;

    fn rules(m_min: f64, m_max: f64) -> PairingRules {
        PairingRules {
            probability: 1.0,
            bar_ratio: 0.618,
            max_upscale: 1.0,
            m_min,
            m_max,
            bevel_max: 0.0,
            lookahead: 1,
        }
    }

    const UHD: Option<(u32, u32)> = Some((3840, 2160));

    #[test]
    fn s4_squares_and_landscapes_are_never_pairable() {
        let r = rules(0.02, 0.06);
        assert!(!r.is_pairable(Some((3000, 3000)), UHD));
        assert!(!r.is_pairable(Some((6000, 4000)), UHD));
        assert!(!r.is_pairable(Some((4000, 2250)), UHD));
    }

    #[test]
    fn s4_common_portrait_shapes_on_16_9() {
        // r = 0.618, 6% mat: limit (16/9 − 2.618 × 0.06) / (2 × 0.88) ≈ 0.921.
        let r = rules(0.06, 0.06);
        let threshold = r.shape_limit((3840, 2160));
        assert!((threshold - 0.921).abs() < 0.001, "{threshold}");
        for (w, h) in [
            (3600, 4000),
            (3200, 4000),
            (3000, 4000),
            (2667, 4000),
            (2250, 4000),
        ] {
            assert!(r.is_pairable(Some((w, h)), UHD), "{w}x{h}");
        }
        assert!(!r.is_pairable(Some((3700, 4000)), UHD), "0.925");
    }

    #[test]
    fn s4_16_10_includes_4_5_but_not_5_6() {
        let r = rules(0.06, 0.06);
        let wuxga = Some((2560, 1600));
        let threshold = r.shape_limit((2560, 1600));
        assert!((threshold - 0.820).abs() < 0.001, "{threshold}");
        assert!(r.is_pairable(Some((3200, 4000)), wuxga), "4:5");
        assert!(!r.is_pairable(Some((3333, 4000)), wuxga), "5:6");
    }

    /// The limit is the tightest over the mats a pair can get: on 16:9 the
    /// smallest margin binds (a wider mat shortens the photos faster than it
    /// narrows the width), on 5:4 the largest does.
    #[test]
    fn s4_shape_limit_takes_the_tightest_mat() {
        let r = rules(0.02, 0.06);
        let at = |m: f64, a: f64| (a - 2.618 * m) / (2.0 * (1.0 - 2.0 * m));
        let uhd = r.shape_limit((3840, 2160));
        assert!((uhd - at(0.02, 16.0 / 9.0)).abs() < 1e-9, "{uhd}");
        assert!(uhd < at(0.06, 16.0 / 9.0));
        let sxga = r.shape_limit((1280, 1024));
        assert!((sxga - at(0.06, 1.25)).abs() < 1e-9, "{sxga}");
        assert!(sxga < at(0.02, 1.25));
        // r = 0: the photos may fill the width edge to edge but for the two
        // borders.
        let edge_to_edge = PairingRules {
            bar_ratio: 0.0,
            ..rules(0.06, 0.06)
        };
        let limit = edge_to_edge.shape_limit((3840, 2160));
        assert!((limit - (16.0 / 9.0 - 0.12) / 1.76).abs() < 1e-9, "{limit}");
    }

    /// Review F4: bevelled mats hold both inner bevels in the bar and the
    /// outer ones in the margins, so the shape limit leaves room for four
    /// (`4·b_max/H_c` of the canvas aspect, `2·b_max/(H_c(1 − 2m))` of the
    /// per-photo limit).
    #[test]
    fn s4_bevels_tighten_the_shape_condition() {
        let plain = rules(0.06, 0.06);
        let bevelled = PairingRules {
            bevel_max: 20.0,
            ..plain
        };
        let beta = 4.0 * 20.0 / (2160.0 * 0.88);
        let edge = plain.shape_limit((3840, 2160));
        // A portrait just inside the plain limit is outside the bevelled one.
        let h = 4000u32;
        let w = ((edge - beta / 4.0) * h as f64).floor() as u32;
        assert!(plain.is_pairable(Some((w, h)), UHD));
        assert!(!bevelled.is_pairable(Some((w, h)), UHD));
        let w = ((edge - beta / 2.0) * h as f64).floor() as u32;
        assert!(bevelled.is_pairable(Some((w, h)), UHD));
    }

    #[test]
    fn s4_full_height_condition_excludes_low_resolution_portraits() {
        // 4K, u = 1, 2% minimum mat: needs h ≥ 2160·0.96 = 2073.6.
        let r = rules(0.02, 0.06);
        assert!(!r.is_pairable(Some((1200, 1600)), UHD));
        assert!(!r.is_pairable(Some((1080, 1350)), UHD));
        assert!(!r.is_pairable(Some((1555, 2073)), UHD));
        assert!(r.is_pairable(Some((1555, 2074)), UHD));
        // u = 2 lets the same 1600 px portrait reach full height.
        let upscaled = PairingRules {
            max_upscale: 2.0,
            ..r
        };
        assert!(upscaled.is_pairable(Some((1200, 1600)), UHD));
    }

    #[test]
    fn s4_nothing_is_pairable_before_the_canvas_is_published() {
        let r = rules(0.02, 0.06);
        assert!(!r.is_pairable(Some((3000, 4000)), None));
        assert!(!r.is_pairable(None, UHD));
    }

    #[test]
    fn s4_margin_range_ignores_zero_margin_options() {
        let yaml = r#"
photo-library-path: /tmp
matting:
  selection: random
  active:
    - kind: fixed-color
      minimum-mat-percentage: 0
    - kind: studio
      minimum-mat-percentage: 3.5
    - kind: blur
      minimum-mat-percentage: 6
playlist:
  pairing:
    enabled: true
"#;
        let cfg: Configuration = serde_yaml::from_str(yaml).expect("parses");
        let cfg = cfg.validated().expect("valid");
        let r = PairingRules::from_config(&cfg).expect("enabled");
        assert!((r.m_min - 0.035).abs() < 1e-6, "{}", r.m_min);
        assert!((r.m_max - 0.06).abs() < 1e-6, "{}", r.m_max);
    }

    fn info(path: &str, display_size: Option<(u32, u32)>) -> PhotoInfo {
        PhotoInfo {
            path: PathBuf::from(path),
            created_at: SystemTime::UNIX_EPOCH,
            created_source: CreatedSource::Birthtime,
            runtime_added: false,
            display_size,
        }
    }

    #[test]
    fn s5_header_fills_only_and_decoded_always_overwrites() {
        let mut sizes = DisplaySizes::default();
        let a = Path::new("a.jpg");
        sizes.on_added(&info("a.jpg", None));
        assert!(sizes.on_meta(a, (3000, 4000), MetaSource::Header));
        assert_eq!(sizes.get(a), Some((3000, 4000)));
        // A second header result never overwrites a Some.
        assert!(!sizes.on_meta(a, (4000, 3000), MetaSource::Header));
        assert_eq!(sizes.get(a), Some((3000, 4000)));
        // A decode always does (an in-place rotation).
        assert!(sizes.on_meta(a, (4000, 3000), MetaSource::Decoded));
        assert_eq!(sizes.get(a), Some((4000, 3000)));
    }

    #[test]
    fn s5_unknown_photo_becomes_pairable_after_its_decode() {
        let r = rules(0.02, 0.06);
        let mut sizes = DisplaySizes::default();
        let a = Path::new("copying.jpg");
        sizes.on_added(&info("copying.jpg", None));
        assert!(!r.is_pairable(sizes.get(a), UHD));
        sizes.on_meta(a, (3000, 4000), MetaSource::Decoded);
        assert!(r.is_pairable(sizes.get(a), UHD));
    }

    #[test]
    fn s5_stale_pass_result_loses_to_a_fresh_photo_added() {
        let mut sizes = DisplaySizes::default();
        let a = Path::new("a.jpg");
        sizes.on_added(&info("a.jpg", None));
        // Replaced in place before the pass reached it: the runtime add read
        // the new header inline.
        sizes.on_added(&PhotoInfo {
            runtime_added: true,
            ..info("a.jpg", Some((3000, 4000)))
        });
        sizes.on_meta(a, (4000, 3000), MetaSource::Header);
        assert_eq!(sizes.get(a), Some((3000, 4000)));
        // A PhotoAdded without a read keeps what is known.
        sizes.on_added(&info("a.jpg", None));
        assert_eq!(sizes.get(a), Some((3000, 4000)));
    }

    #[test]
    fn s5_meta_never_resurrects_a_removed_photo() {
        let mut sizes = DisplaySizes::default();
        let a = Path::new("a.jpg");
        sizes.on_added(&info("a.jpg", Some((3000, 4000))));
        sizes.on_removed(a);
        assert!(!sizes.on_meta(a, (3000, 4000), MetaSource::Decoded));
        assert_eq!(sizes.get(a), None);
        assert_eq!(sizes.iter().count(), 0);
    }
}
