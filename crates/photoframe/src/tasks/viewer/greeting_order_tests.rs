//! The greeting comes first (follow-up 23): at a cold boot the control
//! task's startup state reached the viewer before its first frame and was
//! applied at once, so the greeting was never drawn. These drive the App's
//! mode machine the way the KMS loop does (commands, then control steps),
//! without a display.

use super::*;
use crate::events::ViewerState;

/// Long enough that a stalled test runner cannot let it run out between a
/// present and the step right after it.
const HOLD: Duration = Duration::from_millis(300);
/// Past the end of the hold.
const AFTER_HOLD: Duration = Duration::from_millis(320);

fn app() -> App {
    let cfg = crate::config::Configuration::default();
    let scaling = super::loader::DecodeScaling::from_config(&cfg);
    let (_loader_tx, loader_rx) = crate::tasks::photo_channel::photo_channel(1);
    let (displayed_tx, _displayed_rx) = tokio::sync::mpsc::channel(1);
    let mut app = App::new(
        loader_rx,
        displayed_tx,
        CancellationToken::new(),
        Arc::new(cfg),
        scaling,
    );
    app.max_texture_dimension = Some(4096);
    let size = PhysicalSize::new(1920, 1080);
    app.output = Some(Output::Kms {
        size,
        refresh_period: Duration::from_micros(16_667),
        redraw_requested: Arc::new(AtomicBool::new(false)),
    });
    app.configure_fixed_surface(size);
    app.greeting_hold = greeting_hold::GreetingHold::new(HOLD);
    app
}

/// A 1×1 stand-in for a matted, uploaded photo.
fn photo(device: &wgpu::Device, path: &str) -> ImgTex {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("greeting-order-layout"),
        entries: &[],
    });
    ImgTex {
        plane: TexturePlane {
            bind: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("greeting-order-bind"),
                layout: &layout,
                entries: &[],
            }),
            w: 1,
            h: 1,
            _gpu_bytes: None,
        },
        path: path.into(),
        partner: None,
        mat_kind: None,
    }
}

#[test]
fn the_startup_state_waits_for_the_greeting_to_be_shown_for_its_duration() {
    let mut app = app();
    // Cold boot: the startup state is already queued when the loop starts,
    // before anything has been presented.
    app.handle_control_command(ViewerCommand::SetState(ViewerState::Awake));
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting);
    std::thread::sleep(AFTER_HOLD);
    app.process_tick();
    assert_eq!(
        app.mode_kind(),
        ViewerModeKind::Greeting,
        "the hold starts at the greeting's first present, not at the request"
    );

    app.note_greeting_presented();
    let shown = Instant::now();
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting);

    std::thread::sleep(AFTER_HOLD);
    app.process_tick();
    assert_eq!(
        app.mode_kind(),
        ViewerModeKind::Greeting,
        "awake also waits for a photo: no slideshow on an empty stage"
    );
    let plan = app.wake_plan(Instant::now());
    assert_eq!(plan.deadline, None, "a photo wakes the loop, not a timer");
    assert!(!plan.tick_now);
    assert!(Instant::now() >= shown + HOLD);
}

#[test]
fn the_loop_wakes_when_the_greeting_ends() {
    let mut app = app();
    app.handle_control_command(ViewerCommand::SetState(ViewerState::Asleep));
    app.note_greeting_presented();
    let shown = Instant::now();
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting);
    let deadline = app.wake_plan(Instant::now()).deadline.expect("a deadline");
    assert!(
        deadline <= shown + HOLD,
        "{deadline:?} after {:?}",
        shown + HOLD
    );
}

#[test]
fn asleep_follows_the_greeting_without_a_photo() {
    let mut app = app();
    app.handle_control_command(ViewerCommand::SetState(ViewerState::Asleep));
    app.note_greeting_presented();
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting);
    std::thread::sleep(AFTER_HOLD);
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Sleep);
}

#[test]
fn a_toggle_during_the_greeting_applies_after_the_startup_state() {
    let mut app = app();
    app.note_greeting_presented();
    app.handle_control_command(ViewerCommand::SetState(ViewerState::Awake));
    app.handle_control_command(ViewerCommand::ToggleState);
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting);
    std::thread::sleep(AFTER_HOLD);
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Sleep);
}

#[test]
fn the_slideshow_starts_once_the_greeting_is_over_and_a_photo_is_ready() {
    let Some((_gpu, device, _queue)) = crate::gpu::test_device("greeting-order-test-device") else {
        eprintln!("skipping greeting order test: no GPU adapter available");
        return;
    };
    let mut app = app();
    app.handle_control_command(ViewerCommand::SetState(ViewerState::Awake));
    app.note_greeting_presented();
    // The first photo is ready while the greeting is still on.
    app.mode_mut()
        .wake_mut()
        .pending_mut()
        .push_back(photo(&device, "first.jpg"));
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting);
    assert!(app.mode().wake().current().is_none());

    std::thread::sleep(AFTER_HOLD);
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Wake);
    assert_eq!(
        app.mode().wake().current().map(|img| img.path.clone()),
        Some("first.jpg".into()),
        "the slideshow opens on the photo that was ready"
    );
}

/// Review F1: the greeting's first present happens in the redraw after the
/// step, so with a zero duration (or a step a moment early) the hold is
/// already releasable when the loop plans its wait. The plan must tick,
/// not sleep until an unrelated event.
#[test]
fn a_hold_that_became_releasable_after_the_step_ticks_now() {
    let Some((_gpu, device, _queue)) = crate::gpu::test_device("greeting-race-test-device") else {
        eprintln!("skipping greeting race test: no GPU adapter available");
        return;
    };
    let mut app = app();
    app.greeting_hold = greeting_hold::GreetingHold::new(Duration::ZERO);
    app.handle_control_command(ViewerCommand::SetState(ViewerState::Awake));
    app.mode_mut()
        .wake_mut()
        .pending_mut()
        .push_back(photo(&device, "first.jpg"));
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Greeting, "not shown yet");
    // The loop's redraw presents the greeting.
    app.note_greeting_presented();
    let plan = app.wake_plan(Instant::now());
    assert!(
        plan.tick_now,
        "a releasable hold ticks now: deadline={:?} redraw_now={}",
        plan.deadline, plan.redraw_now
    );
    app.process_tick();
    assert_eq!(app.mode_kind(), ViewerModeKind::Wake);
}
