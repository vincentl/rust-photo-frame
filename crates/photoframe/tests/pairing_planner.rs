//! Portrait pairing's screen planner, exercised through `simulate_screens`
//! (local-markdown/p1-pairing-design.md, test plan S1-S6). S7 and S8 are unit
//! tests beside the planner (`tasks/manager/planner.rs`).

use photoframe::config::{PlaylistOptions, PlaylistOrder};
use photoframe::events::{CreatedSource, PhotoInfo};
use photoframe::tasks::manager::pairing::PairingRules;
use photoframe::tasks::manager::planner::{PlannedScreen, ScreenStats};
use photoframe::tasks::manager::{simulate_playlist, simulate_screens};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CANVAS: Option<(u32, u32)> = Some((3840, 2160));
const PORTRAIT: (u32, u32) = (3000, 4000);
const LANDSCAPE: (u32, u32) = (4000, 3000);

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

fn options(f: f64) -> PlaylistOptions {
    PlaylistOptions {
        new_multiplicity: 3,
        half_life: Duration::from_secs(86_400),
        order: PlaylistOrder::WeightedSpread,
        min_spacing: f,
        ..Default::default()
    }
}

fn rules(probability: f64) -> PairingRules {
    PairingRules {
        probability,
        bar_ratio: 0.618,
        max_upscale: 1.0,
        m_min: 0.02,
        m_max: 0.06,
        bevel_max: 0.0,
    }
}

/// One photo per entry: `(weight 1 or 3, pairable, runtime-added)`. Weight 3
/// is a photo created `now` (new-multiplicity 3, frozen clock); weight 1 is a
/// year old.
#[derive(Clone, Copy)]
struct Spec {
    weight3: bool,
    pairable: bool,
    runtime: bool,
}

fn library(specs: &[Spec]) -> Vec<PhotoInfo> {
    specs
        .iter()
        .enumerate()
        .map(|(i, spec)| PhotoInfo {
            path: PathBuf::from(format!("p{i:03}.jpg")),
            created_at: if spec.weight3 {
                now()
            } else {
                now() - Duration::from_secs(365 * 86_400)
            },
            created_source: CreatedSource::Birthtime,
            runtime_added: spec.runtime,
            display_size: Some(if spec.pairable { PORTRAIT } else { LANDSCAPE }),
        })
        .collect()
}

fn specs(n: usize, f: impl Fn(usize) -> Spec) -> Vec<Spec> {
    (0..n).map(f).collect()
}

fn plain(pairable: bool) -> Spec {
    Spec {
        weight3: false,
        pairable,
        runtime: false,
    }
}

struct Lib {
    label: &'static str,
    photos: Vec<PhotoInfo>,
    f: f64,
}

/// S1's libraries: uniform; 1:3 weights at f = 0 and 0.7; runtime-added
/// photos in the FIFO; a 3-photo library where bumps fire; plus S6's.
fn s1_libraries() -> Vec<Lib> {
    let every_third_landscape = |i: usize| i % 3 != 0;
    vec![
        Lib {
            label: "uniform 30, f=0.7",
            photos: library(&specs(30, |i| plain(every_third_landscape(i)))),
            f: 0.7,
        },
        Lib {
            label: "1:3 weights, f=0",
            photos: library(&specs(20, |i| Spec {
                weight3: i % 2 == 0,
                ..plain(every_third_landscape(i))
            })),
            f: 0.0,
        },
        Lib {
            label: "1:3 weights, f=0.7",
            photos: library(&specs(20, |i| Spec {
                weight3: i % 2 == 0,
                ..plain(every_third_landscape(i))
            })),
            f: 0.7,
        },
        Lib {
            label: "runtime-added in the FIFO, f=0.7",
            photos: library(&specs(20, |i| Spec {
                weight3: i % 2 == 0,
                runtime: i >= 15,
                ..plain(every_third_landscape(i))
            })),
            f: 0.7,
        },
        Lib {
            label: "6 photos with FIFO, f=0",
            photos: library(&specs(6, |i| Spec {
                weight3: i >= 3,
                runtime: i >= 4,
                ..plain(true)
            })),
            f: 0.0,
        },
        Lib {
            label: "3 photos, bumps fire, f=0.7",
            photos: library(&specs(3, |i| Spec {
                weight3: i == 2,
                ..plain(true)
            })),
            f: 0.7,
        },
    ]
    .into_iter()
    .chain(s6_libraries())
    .collect()
}

/// S6's libraries, where grouping without rules 4 and 5 puts a photo on
/// consecutive screens.
fn s6_libraries() -> Vec<Lib> {
    vec![
        Lib {
            label: "f=0, 20 portraits",
            photos: library(&specs(20, |_| plain(true))),
            f: 0.0,
        },
        Lib {
            label: "10-photo album, one at w=3, f=0.7",
            photos: library(&specs(10, |i| Spec {
                weight3: i == 0,
                ..plain(true)
            })),
            f: 0.7,
        },
        Lib {
            label: "f=0, 30 + 30",
            photos: library(&specs(60, |i| plain(i < 30))),
            f: 0.0,
        },
    ]
}

fn flatten(screens: &[PlannedScreen]) -> Vec<PathBuf> {
    screens
        .iter()
        .flat_map(PlannedScreen::members)
        .map(|m| (*m.path).clone())
        .collect()
}

fn pairable_set(photos: &[PhotoInfo]) -> HashSet<PathBuf> {
    photos
        .iter()
        .filter(|info| rules(1.0).is_pairable(info.display_size, CANVAS))
        .map(|info| info.path.clone())
        .collect()
}

fn screens(lib: &Lib, probability: Option<f64>, n: usize, seed: u64) -> Vec<PlannedScreen> {
    let rules = probability.map(rules);
    simulate_screens(
        lib.photos.clone(),
        options(lib.f),
        rules.as_ref(),
        CANVAS,
        now(),
        n,
        Some(seed),
    )
}

/// **S1 Identity (exact, executes P1).** The flattened screens equal the
/// unpaired sequence, bumps and the FIFO included, for every p.
#[test]
fn s1_flattened_screens_equal_the_unpaired_sequence() {
    for lib in s1_libraries() {
        for seed in 0..5 {
            for p in [None, Some(0.0), Some(0.5), Some(1.0)] {
                let flat = flatten(&screens(&lib, p, 3_000, seed));
                let unpaired = simulate_playlist(
                    lib.photos.clone(),
                    options(lib.f),
                    now(),
                    flat.len(),
                    Some(seed),
                );
                assert_eq!(flat, unpaired, "{} seed {seed} p {p:?}", lib.label);
            }
        }
    }
}

/// Pairing off is exactly today: every screen solo.
#[test]
fn pairing_off_and_p0_plan_only_solo_screens() {
    for lib in s1_libraries() {
        for p in [None, Some(0.0)] {
            let planned = screens(&lib, p, 2_000, 7);
            assert!(
                planned.iter().all(|s| s.partner.is_none()),
                "{} p {p:?}",
                lib.label
            );
        }
    }
}

/// **S2 Grouping.** Every pair is two consecutive pairable appearances with
/// X ≠ P; no non-pairable photo is ever in a pair.
#[test]
fn s2_pairs_are_consecutive_pairable_appearances() {
    for lib in s1_libraries() {
        let pairable = pairable_set(&lib.photos);
        for p in [0.5, 1.0] {
            let planned = screens(&lib, Some(p), 5_000, 11);
            let mut pairs = 0;
            for screen in &planned {
                if let Some(partner) = &screen.partner {
                    pairs += 1;
                    assert!(
                        pairable.contains(screen.primary.path.as_ref()),
                        "{}",
                        lib.label
                    );
                    assert!(pairable.contains(partner.path.as_ref()), "{}", lib.label);
                    assert_ne!(screen.primary.path, partner.path, "{}", lib.label);
                }
            }
            if !pairable.is_empty() && lib.photos.len() > 3 {
                assert!(pairs > 0, "{} p {p}: no pairs at all", lib.label);
            }
        }
    }
}

/// **S3 Rate.** On a large library the measured paired share is within ±5%
/// relative of 2πp/(1+πp); p = 0 gives no pairs.
#[test]
fn s3_paired_share_matches_the_rate_model_on_a_large_library() {
    // 400 portraits / 600 landscapes at equal weight: π = 0.4.
    let lib = Lib {
        label: "400 + 600",
        photos: library(&specs(1000, |i| plain(i < 400))),
        f: 0.7,
    };
    let pairable = pairable_set(&lib.photos);
    let is_pairable = |path: &Path| pairable.contains(path);
    for p in [1.0, 0.5] {
        let planned = screens(&lib, Some(p), 100_000, 3);
        // Skip the first lap, where the startup order is not yet mixed.
        let stats = ScreenStats::measure(&planned[1_000..], &is_pairable);
        let predicted = ScreenStats::predicted_share(0.4, p);
        let relative = (stats.paired_share - predicted).abs() / predicted;
        assert!(
            relative <= 0.05,
            "p {p}: measured {:.3} vs predicted {predicted:.3}",
            stats.paired_share
        );
    }
    let planned = screens(&lib, Some(0.0), 20_000, 3);
    assert_eq!(ScreenStats::measure(&planned, &is_pairable).pair_screens, 0);
}

/// **S6 No consecutive repeats (I5).** A photo is on two consecutive screens
/// only where the unpaired sequence itself has it twice in a row (Z, Z): the
/// first screen ends with Z and the next begins with it. Over 100k
/// appearances, several seeds and p in {0.5, 1}, on the review's libraries
/// (where grouping without rules 4 and 5 repeats) plus FIFO libraries at
/// f = 0, where debuts do produce Z, Z, so the exception is exercised.
#[test]
fn s6_no_photo_on_consecutive_screens_beyond_the_sequences_own_z_z() {
    let fifo = vec![
        Lib {
            label: "6 portraits, 3 FIFO debuts, f=0",
            photos: library(&specs(6, |i| Spec {
                runtime: i >= 3,
                weight3: i >= 3,
                ..plain(true)
            })),
            f: 0.0,
        },
        Lib {
            label: "12 portraits, 6 FIFO debuts, f=0",
            photos: library(&specs(12, |i| Spec {
                runtime: i >= 6,
                weight3: true,
                ..plain(true)
            })),
            f: 0.0,
        },
    ];
    let mut z_z_seen = 0;
    for lib in s6_libraries().into_iter().chain(fifo) {
        let pairable = pairable_set(&lib.photos);
        let is_pairable = |path: &Path| pairable.contains(path);
        for seed in 0..4 {
            for p in [0.5, 1.0] {
                let planned = screens(&lib, Some(p), 100_000, seed);
                let stats = ScreenStats::measure(&planned, &is_pairable);
                assert!(stats.pair_screens > 0, "{} seed {seed} p {p}", lib.label);
                for w in planned.windows(2) {
                    let shared: Vec<_> =
                        w[1].members().filter(|m| w[0].contains(&m.path)).collect();
                    for z in shared {
                        let last = w[0].members().last().unwrap();
                        assert!(
                            last.path == z.path && w[1].primary.path == z.path,
                            "{} seed {seed} p {p}: {:?} then {:?} is not a Z, Z",
                            lib.label,
                            w[0],
                            w[1]
                        );
                        z_z_seen += 1;
                    }
                }
            }
        }
    }
    assert!(z_z_seen > 0, "no library exercised the Z, Z exception");
}

/// Before the canvas is published nothing pairs, and the sequence is today's.
#[test]
fn nothing_pairs_without_a_canvas() {
    let lib = &s6_libraries()[0];
    let planned = simulate_screens(
        lib.photos.clone(),
        options(lib.f),
        Some(&rules(1.0)),
        None,
        now(),
        500,
        Some(5),
    );
    assert!(planned.iter().all(|s| s.partner.is_none()));
}

/// Writes the unpaired sequences and the planner's screens for the
/// cross-check against the review's reference planner (`b2b_planner.py`):
/// `PAIRING_CROSSCHECK_OUT=/path/out.json cargo test -p photoframe --test
/// pairing_planner -- --ignored`. One JSON object per line: library, seed,
/// pairable flags, the unpaired sequence and the screens (photo indices).
#[test]
#[ignore]
fn export_screens_for_the_reference_planner() {
    use std::io::Write;
    let Ok(out) = std::env::var("PAIRING_CROSSCHECK_OUT") else {
        return;
    };
    let mut file = std::fs::File::create(out).unwrap();
    let index = |path: &Path| -> usize { path.to_str().unwrap()[1..4].parse().unwrap() };
    for lib in s1_libraries() {
        let pairable = pairable_set(&lib.photos);
        let flags: Vec<bool> = lib
            .photos
            .iter()
            .map(|i| pairable.contains(&i.path))
            .collect();
        for seed in 0..3 {
            let planned = screens(&lib, Some(1.0), 20_000, seed);
            let flat = flatten(&planned);
            let unpaired: Vec<usize> = flat.iter().map(|p| index(p)).collect();
            let grouped: Vec<Vec<usize>> = planned
                .iter()
                .map(|s| s.members().map(|m| index(&m.path)).collect())
                .collect();
            writeln!(
                file,
                "{{\"label\":{:?},\"seed\":{seed},\"pairable\":{flags:?},\"sequence\":{unpaired:?},\"screens\":{grouped:?}}}",
                lib.label
            )
            .unwrap();
        }
    }
}
