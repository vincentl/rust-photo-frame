//! The canvas reaches `DecodeScaling` (and portrait pairing's `CanvasShape`)
//! once the surface becomes ready, on both display paths. A regression here
//! (the K1 merge left the publish unreachable) silently disables D3's scaled
//! decode and portrait pairing.

use super::*;
use crate::events::CanvasShape;

fn app() -> (App, super::loader::DecodeScaling, CanvasShape) {
    let cfg = crate::config::Configuration::default();
    let shape = CanvasShape::default();
    let scaling = super::loader::DecodeScaling::from_config(&cfg).with_canvas_shape(shape.clone());
    let (_loader_tx, loader_rx) = tokio::sync::mpsc::channel(1);
    let (displayed_tx, _displayed_rx) = tokio::sync::mpsc::channel(1);
    let mut app = App::new(
        loader_rx,
        displayed_tx,
        CancellationToken::new(),
        Arc::new(cfg),
        scaling.clone(),
    );
    // What `install_gpu` records from the GPU's limits.
    app.max_texture_dimension = Some(4096);
    (app, scaling, shape)
}

#[test]
fn wayland_handshake_publishes_the_canvas_once_the_surface_is_ready() {
    let (mut app, scaling, shape) = app();
    // `install_gpu`'s steps, then the compositor's configure events.
    app.surface_gate.reset();
    app.surface_gate.arm_for_initial_report();
    app.update_surface_ready(1920, 1080, SurfaceReport::InitialConfig);
    app.update_surface_ready(1920, 1080, SurfaceReport::ConfigureEvent);
    assert_eq!(shape.get(), None, "not ready before the stable dwell");
    assert_eq!(scaling.target(), None);
    std::thread::sleep(SURFACE_READY_DWELL + Duration::from_millis(20));
    assert!(app.update_surface_ready(1920, 1080, SurfaceReport::ConfigureEvent));
    assert_eq!(shape.get(), Some((1920, 1080)));
    let target = scaling.target().expect("scaled decode targets the canvas");
    assert_eq!((target.canvas_w, target.canvas_h), (1920, 1080));
}

#[test]
fn kms_configure_publishes_the_canvas() {
    let (mut app, scaling, shape) = app();
    app.configure_fixed_surface(PhysicalSize::new(3840, 2160));
    assert_eq!(shape.get(), Some((3840, 2160)));
    let target = scaling.target().expect("scaled decode targets the canvas");
    assert_eq!((target.canvas_w, target.canvas_h), (3840, 2160));
}
