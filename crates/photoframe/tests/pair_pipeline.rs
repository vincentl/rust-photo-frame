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

/// Every canvas the queued work produces. Waits until nothing is left in the
/// pipeline: `wait_for_ready_results` returns at the first result, and the
/// single matting worker may still be busy with the next screen, so a fixed
/// sleep after it is a race on a loaded machine.
async fn ready(harness: &mut MattingQueueHarness) -> Vec<(PathBuf, Option<PathBuf>)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut out = Vec::new();
    loop {
        harness.wait_for_ready_results(Duration::from_secs(5)).await;
        out.extend(
            harness
                .take_ready_canvases()
                .into_iter()
                .map(|c| (c.path, c.partner.map(|(p, _)| p))),
        );
        if harness.matting_inflight() == 0 {
            return out;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "matting did not finish"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pair_is_matted_into_one_canvas() {
    let mut h = harness(2);
    h.push_deferred_pair((photo("a", 120, 160), false), (photo("b", 96, 160), false));
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
    h.push_deferred(photo("b", 96, 160), true);
    h.push_deferred_pair((photo("a", 120, 160), false), (photo("b", 96, 160), false));
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
    h.push_deferred_pair((photo("a", 120, 160), false), (photo("b", 96, 160), false));
    h.queue_once();
    h.wait_for_ready_results(Duration::from_secs(5)).await;
    h.push_deferred(photo("b", 96, 160), true);
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
        (photo("heap", 120, 160), false),
        (photo("debut", 96, 160), true),
    );
    // A stale, non-priority copy of the debut queued behind it.
    h.push_deferred(photo("debut", 96, 160), false);
    h.queue_once();
    assert_eq!(h.deferred_queue_len(), 0, "the stale copy was purged");
    h.wait_for_ready_results(Duration::from_secs(5)).await;
    let canvases = h.take_ready_canvases();
    assert_eq!(canvases.len(), 1);
    let pair = &canvases[0];
    assert!(!pair.priority);
    assert_eq!(pair.partner, Some((PathBuf::from("debut"), true)));
}

/// Review F9: a pair planned for another canvas (a mode change, a rotation)
/// is not drawn width-bound; it becomes two solo screens, in order, each
/// member keeping its priority.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pair_unpairable_on_this_canvas_is_split_into_two_solo_screens() {
    let mut h = harness(3);
    // 30 x 40 cannot reach the 158 px full height of a 180 px canvas.
    h.push_deferred_pair((photo("a", 30, 40), false), (photo("b", 24, 40), true));
    h.queue_once();
    h.wait_for_ready_results(Duration::from_secs(5)).await;
    let mut canvases = Vec::new();
    for _ in 0..50 {
        h.drain_pipeline();
        canvases.extend(h.take_ready_canvases());
        if canvases.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let got: Vec<_> = canvases
        .iter()
        .map(|c| (c.path.clone(), c.priority, c.partner.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (PathBuf::from("a"), false, None),
            (PathBuf::from("b"), true, None)
        ]
    );
}
