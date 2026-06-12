//! Offline validation of the WGSL shaders that are otherwise only compiled at
//! viewer startup, so parse/type/uniformity errors surface in `cargo test`
//! instead of on the frame.

fn validate(name: &str, source: &str) {
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|err| panic!("{name} failed to parse: {}", err.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|err| panic!("{name} failed validation: {err:?}"));
}

#[test]
fn viewer_quad_wgsl_validates() {
    validate(
        "viewer_quad.wgsl",
        include_str!("../src/tasks/shaders/viewer_quad.wgsl"),
    );
}

#[test]
fn blur_bg_wgsl_validates() {
    validate(
        "blur_bg.wgsl",
        include_str!("../src/tasks/shaders/blur_bg.wgsl"),
    );
}

#[test]
fn greeting_frame_wgsl_validates() {
    validate(
        "greeting_frame.wgsl",
        include_str!("../src/tasks/greeting_frame.wgsl"),
    );
}

#[test]
fn caption_composite_wgsl_validates() {
    validate(
        "caption_composite.wgsl",
        include_str!("../src/tasks/viewer/scenes/caption_composite.wgsl"),
    );
}
