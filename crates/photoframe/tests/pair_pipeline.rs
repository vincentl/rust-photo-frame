//! Portrait pairing through the viewer's matting queue (design test plan,
//! "Pipeline"): one canvas per pair, the per-member priority (B2) and the
//! purge by membership (R14).

use std::path::PathBuf;
use std::time::Duration;

use photoframe::config::{Configuration, TransitionConfig};
use photoframe::events::PreparedImageCpu;
use photoframe::tasks::viewer::testkit::MattingQueueHarness;

fn harness(preload: usize) -> MattingQueueHarness {
    let cfg: Configuration = serde_yaml::from_str(
        "photo-library-path: /photos\nmatting:\n  selection: fixed\n  active:\n    - kind: fixed-color\n      minimum-mat-percentage: 6\n",
    )
    .unwrap();
    let cfg = cfg.validated().unwrap();
    let mut harness = MattingQueueHarness::new(
        preload,
        1.0,
        cfg.matting.clone(),
        5_000,
        TransitionConfig::default(),
    );
    harness.update_surface_state(Some((320, 180, 4096)));
    harness.set_surface_configured(true);
    harness
}

fn photo(name: &str, w: u32, h: u32) -> PreparedImageCpu {
    PreparedImageCpu {
        path: PathBuf::from(name),
        width: w,
        height: h,
        source_size: (w, h),
        pixels: vec![200; (w * h * 4) as usize],
    }
}

async fn ready(harness: &mut MattingQueueHarness) -> Vec<(PathBuf, Option<PathBuf>)> {
    harness.wait_for_ready_results(Duration::from_secs(5)).await;
    let mut out = Vec::new();
    // Up to a few results may still be in flight.
    for _ in 0..50 {
        out.extend(
            harness
                .take_ready_canvases()
                .into_iter()
                .map(|c| (c.path, c.partner.map(|(p, _)| p))),
        );
        if harness.deferred_queue_len() == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            harness.drain_pipeline();
            out.extend(
                harness
                    .take_ready_canvases()
                    .into_iter()
                    .map(|c| (c.path, c.partner.map(|(p, _)| p))),
            );
            break;
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pair_is_matted_into_one_canvas() {
    let mut h = harness(2);
    h.push_deferred_pair((photo("a", 30, 40), false), (photo("b", 24, 40), false));
    h.queue_once();
    h.wait_for_ready_results(Duration::from_secs(5)).await;
    let canvases = h.take_ready_canvases();
    assert_eq!(canvases.len(), 1);
    let canvas = &canvases[0];
    assert_eq!(canvas.path, PathBuf::from("a"));
    assert_eq!(canvas.partner, Some((PathBuf::from("b"), false)));
    assert_eq!((canvas.width, canvas.height), (320, 180));
}

/// R14 before matting: a queued pair containing a priority photo degrades
/// to its other member.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_priority_photo_degrades_a_queued_pair_containing_it() {
    let mut h = harness(3);
    h.push_deferred(photo("b", 24, 40), true);
    h.push_deferred_pair((photo("a", 30, 40), false), (photo("b", 24, 40), false));
    h.queue_once();
    let mut got = ready(&mut h).await;
    got.sort();
    assert_eq!(
        got,
        [(PathBuf::from("a"), None), (PathBuf::from("b"), None)],
        "the pair (a, b) degraded to a; b matted once, as the priority photo"
    );
}

/// R14 after compositing: a matted pair canvas containing a priority photo
/// is dropped (`pair_purged`), the loss today's purge imposes on the photo.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_priority_photo_drops_a_matted_pair_containing_it() {
    let mut h = harness(3);
    h.push_deferred_pair((photo("a", 30, 40), false), (photo("b", 24, 40), false));
    h.queue_once();
    h.wait_for_ready_results(Duration::from_secs(5)).await;
    h.push_deferred(photo("b", 24, 40), true);
    h.queue_once();
    let got = ready(&mut h).await;
    assert_eq!(got, [(PathBuf::from("b"), None)]);
}

/// B2: a (heap photo, debut) pair is a priority screen, and the debut's
/// purge fires although it comes second.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_debut_second_in_a_pair_keeps_its_priority() {
    let mut h = harness(3);
    h.push_deferred_pair(
        (photo("heap", 30, 40), false),
        (photo("debut", 24, 40), true),
    );
    // A stale, non-priority copy of the debut queued behind it.
    h.push_deferred(photo("debut", 24, 40), false);
    h.queue_once();
    assert_eq!(h.deferred_queue_len(), 0, "the stale copy was purged");
    h.wait_for_ready_results(Duration::from_secs(5)).await;
    let canvases = h.take_ready_canvases();
    assert_eq!(canvases.len(), 1);
    let pair = &canvases[0];
    assert!(!pair.priority);
    assert_eq!(pair.partner, Some((PathBuf::from("debut"), true)));
}
