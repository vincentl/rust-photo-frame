use std::path::PathBuf;
use std::time::Duration;

use photo_frame::config::{MattingConfig, TransitionConfig};
use photo_frame::events::PreparedImageCpu;
use photo_frame::tasks::viewer::testkit::{MattingQueueHarness, compute_canvas_size_for_test};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_defers_matting_until_surface_ready_event() {
    let mut matting = MattingConfig::default();
    matting.prepare_runtime().expect("matting config ready");

    let mut harness = MattingQueueHarness::new(2, 1.0, matting, 5_000, TransitionConfig::default());

    harness.update_surface_state(Some((800, 600, 4096)));

    harness.push_deferred(
        PreparedImageCpu {
            path: PathBuf::from("/photos/startup.jpg"),
            width: 1600,
            height: 1067,
            pixels: vec![180; (1600 * 1067 * 4) as usize],
        },
        false,
    );

    harness.queue_once();
    harness
        .wait_for_ready_results(Duration::from_millis(100))
        .await;
    assert!(
        harness.take_ready_canvases().is_empty(),
        "matting should be deferred until the surface is confirmed ready",
    );
    assert_eq!(
        harness.deferred_queue_len(),
        1,
        "deferred queue should retain the pending image while waiting for resize",
    );

    harness.update_surface_state(Some((1920, 1080, 4096)));
    harness.set_surface_configured(true);

    harness.queue_once();
    harness.wait_for_ready_results(Duration::from_secs(2)).await;
    let ready = harness.take_ready_canvases();
    assert_eq!(ready.len(), 1, "expected one matting result once ready");
    let expected = compute_canvas_size_for_test(1920, 1080, 1.0, 4096);
    assert_eq!((ready[0].width, ready[0].height), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_reconfigures_after_surface_loss() {
    let mut matting = MattingConfig::default();
    matting.prepare_runtime().expect("matting config ready");

    let mut harness = MattingQueueHarness::new(1, 1.0, matting, 5_000, TransitionConfig::default());

    harness.update_surface_state(Some((800, 600, 4096)));
    harness.set_surface_configured(true);
    harness.push_deferred(
        PreparedImageCpu {
            path: PathBuf::from("/photos/startup.jpg"),
            width: 1600,
            height: 1067,
            pixels: vec![180; (1600 * 1067 * 4) as usize],
        },
        false,
    );
    harness.queue_once();
    harness.wait_for_ready_results(Duration::from_secs(2)).await;
    let initial_ready = harness.take_ready_canvases();
    assert_eq!(initial_ready.len(), 1, "expected initial matting result");
    let small_expected = compute_canvas_size_for_test(800, 600, 1.0, 4096);
    assert_eq!(
        (initial_ready[0].width, initial_ready[0].height),
        small_expected
    );

    harness.set_surface_configured(false);
    harness.push_deferred(
        PreparedImageCpu {
            path: PathBuf::from("/photos/upgrade.jpg"),
            width: 2000,
            height: 1125,
            pixels: vec![220; (2000 * 1125 * 4) as usize],
        },
        false,
    );
    harness.queue_once();
    harness
        .wait_for_ready_results(Duration::from_millis(100))
        .await;
    assert!(
        harness.take_ready_canvases().is_empty(),
        "matting should defer while the surface is unconfigured",
    );
    assert_eq!(
        harness.deferred_queue_len(),
        1,
        "deferred queue should retain the image until reconfiguration",
    );

    harness.update_surface_state(Some((1920, 1080, 4096)));
    harness.set_surface_configured(true);
    harness.queue_once();
    harness.wait_for_ready_results(Duration::from_secs(2)).await;
    harness.queue_once();
    harness.drain_pipeline();
    assert_eq!(
        harness.deferred_queue_len(),
        0,
        "deferred queue should be empty after reconfiguration",
    );
    let ready = harness.take_ready_canvases();
    assert!(
        ready.len() <= 1,
        "unexpected multiple matting results after reconfigure"
    );
    if let Some(result) = ready.first() {
        let expected = compute_canvas_size_for_test(1920, 1080, 1.0, 4096);
        assert_eq!((result.width, result.height), expected);
    }
}
