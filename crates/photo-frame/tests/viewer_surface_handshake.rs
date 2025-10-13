use std::path::PathBuf;
use std::time::Duration;

use rust_photo_frame::config::{MattingConfig, TransitionConfig};
use rust_photo_frame::events::PreparedImageCpu;
use rust_photo_frame::tasks::viewer::testkit::{MattingQueueHarness, compute_canvas_size_for_test};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_defers_matting_until_surface_ready_event() {
    let mut matting = MattingConfig::default();
    // Ensure deterministic option ordering and validate defaults are ready.
    matting.prepare_runtime().expect("matting config ready");

    let mut harness = MattingQueueHarness::new(1, 1.0, matting, 5_000, TransitionConfig::default());

    harness.update_surface_state(Some((800, 600, 4096)));
    harness.arm_surface_gate();
    harness.report_surface_initial_config(800, 600);

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
    harness.report_surface_configured(1920, 1080);

    harness.queue_once();
    harness.wait_for_ready_results(Duration::from_secs(2)).await;
    let ready = harness.take_ready_canvases();
    assert_eq!(ready.len(), 1, "expected one matting result once ready");
    let expected = compute_canvas_size_for_test(1920, 1080, 1.0, 4096);
    assert_eq!((ready[0].width, ready[0].height), expected);
}
