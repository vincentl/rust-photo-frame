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
/// - `r` = `pairing.bar-ratio`; `g_max = r·m_max/(1 − 2·m_max)` is the largest
///   center bar as a fraction of photo height;
/// - `b_max` = the widest bevel among the non-zero mats (studio and
///   passe-partout cut one around each window; the bar adds both bevels to
///   its flat `r × border`, see `viewer/photo_set.rs`).
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
}

impl PairingRules {
    /// `None` when pairing is disabled (or, impossible after validation, no
    /// non-zero mat is configured).
    pub fn from_config(cfg: &Configuration) -> Option<Self> {
        let pairing = &cfg.playlist.pairing;
        if !pairing.enabled {
            return None;
        }
        Self::from_parts(
            pairing,
            &cfg.matting,
            cfg.global_photo_settings.max_upscale_factor,
        )
    }

    /// The rules for `pairing` with these mats and max-upscale factor (the
    /// viewer's check before drawing a pair, and test harnesses). `None` when
    /// no non-zero mat is configured.
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
        })
    }

    /// Center bar as a fraction of photo height for a mat of margin `m`:
    /// `g = r·m/(1 − 2m)`, so that at photo height `H_c(1 − 2m)` the bar is
    /// exactly `r × border` (`border = H_c·m`).
    pub fn bar_fraction(bar_ratio: f64, m: f64) -> f64 {
        bar_ratio * m / (1.0 - 2.0 * m)
    }

    /// `g_max`, the largest center bar a pair screen can get.
    pub fn g_max(&self) -> f64 {
        Self::bar_fraction(self.bar_ratio, self.m_max)
    }

    /// **Pairable (R2).** Photo `i` (displayed size `w_i x h_i`, aspect
    /// `a_i = w_i/h_i`) is pairable iff
    /// - the canvas `(W_c, H_c)` has been published (`A = W_c/H_c`),
    /// - its size is known,
    /// - **shape condition:** `a_i ≤ (A − g_max − β)/2`, with the bevel term
    ///   `β = 4·b_max / (H_c(1 − 2·m_max))` (0 without bevelled mats), and
    /// - **full-height condition:** `h_i·u ≥ H_c(1 − 2·m_min)`.
    ///
    /// Together these give the no-shrink invariant C2: each member of a pair
    /// is drawn at exactly the height it would have alone, for every mat a
    /// pair can get (proof in the design, §1). The bevel term extends the
    /// shape step to studio and passe-partout, whose bar is `g·H + 2b` and
    /// whose photo space is `D − 2b` tall and `A·D − 2b` wide (`D =
    /// H_c(1 − 2m)`): the pair fits that width at full height when
    /// `Σa + g ≤ (A·D − 4b)/(D − 2b)`, which `Σa + g ≤ A − 4b/D` implies, and
    /// `4b/D ≤ β` for every non-zero mat.
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
        let canvas_aspect = f64::from(canvas_w) / f64::from(canvas_h);
        let bevel_term = 4.0 * self.bevel_max / (f64::from(canvas_h) * (1.0 - 2.0 * self.m_max));
        let shape = a <= (canvas_aspect - self.g_max() - bevel_term) / 2.0;
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
        // r = 0.618, 6% mat: threshold (16/9 − g_max)/2 ≈ 0.868.
        let r = rules(0.06, 0.06);
        let threshold = (16.0 / 9.0 - r.g_max()) / 2.0;
        assert!((threshold - 0.868).abs() < 0.001, "{threshold}");
        for (w, h) in [(3200, 4000), (3000, 4000), (2667, 4000), (2250, 4000)] {
            assert!(r.is_pairable(Some((w, h)), UHD), "{w}x{h}");
        }
    }

    #[test]
    fn s4_16_10_excludes_4_5() {
        let r = rules(0.06, 0.06);
        let wuxga = Some((2560, 1600));
        let threshold = (1.6 - r.g_max()) / 2.0;
        assert!((threshold - 0.779).abs() < 0.001, "{threshold}");
        assert!(!r.is_pairable(Some((3200, 4000)), wuxga), "4:5");
        assert!(r.is_pairable(Some((3000, 4000)), wuxga), "3:4");
    }

    /// Review F4: bevelled mats add both bevels to the bar, so the shape
    /// condition leaves room for them (`β = 4·b_max/(H_c(1 − 2·m_max))`).
    #[test]
    fn s4_bevels_tighten_the_shape_condition() {
        let plain = rules(0.06, 0.06);
        let bevelled = PairingRules {
            bevel_max: 20.0,
            ..plain
        };
        let beta = 4.0 * 20.0 / (2160.0 * 0.88);
        let edge = (16.0 / 9.0 - plain.g_max()) / 2.0;
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
