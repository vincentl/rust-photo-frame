use std::time::{Duration, Instant};

use rust_photo_frame::config::AwakeScheduleConfig;
use rust_photo_frame::events::{ViewerCommand, ViewerState};
use rust_photo_frame::schedule;
use serde_yaml::from_str;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn schedule_from_yaml(input: &str) -> AwakeScheduleConfig {
    let mut schedule: AwakeScheduleConfig = from_str(input).expect("valid schedule yaml");
    schedule.validate().expect("valid schedule");
    schedule
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_defers_initial_wake_for_greeting() {
    let schedule = schedule_from_yaml(
        r#"
timezone: "UTC"
awake-scheduled:
  daily:
    - ["00:00:00", "23:59:59"]
"#,
    );

    let (tx, mut rx) = mpsc::channel::<ViewerCommand>(4);
    let cancel = CancellationToken::new();
    let greeting_delay = Duration::from_millis(250);

    let start = Instant::now();
    let handle = tokio::spawn(schedule::run(schedule, cancel.clone(), tx, greeting_delay));

    let early = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        early.is_err(),
        "command should not arrive before greeting delay elapses"
    );

    let command = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("expected wake command after greeting delay")
        .expect("schedule channel closed unexpectedly");

    let elapsed = start.elapsed();
    match command {
        ViewerCommand::SetState(ViewerState::Awake) => {
            assert!(
                elapsed >= greeting_delay,
                "awake command sent too early: {:?} < {:?}",
                elapsed,
                greeting_delay
            );
        }
        other => panic!("unexpected command: {:?}", other),
    }

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schedule_does_not_auto_wake_during_sleep_interval() {
    let schedule = schedule_from_yaml(
        r#"
timezone: "UTC"
"#,
    );

    let (tx, mut rx) = mpsc::channel::<ViewerCommand>(4);
    let cancel = CancellationToken::new();
    let greeting_delay = Duration::from_millis(250);

    let handle = tokio::spawn(schedule::run(schedule, cancel.clone(), tx, greeting_delay));

    let first_command = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("expected immediate sleep command")
        .expect("schedule channel closed unexpectedly");

    assert_eq!(
        first_command,
        ViewerCommand::SetState(ViewerState::Asleep),
        "schedule should enforce sleep state immediately"
    );

    let stray_wake = tokio::time::timeout(Duration::from_millis(400), async {
        while let Some(cmd) = rx.recv().await {
            if matches!(cmd, ViewerCommand::SetState(ViewerState::Awake)) {
                return Some(cmd);
            }
        }
        None
    })
    .await;

    assert!(
        stray_wake.is_err() || stray_wake.unwrap().is_none(),
        "sleep interval was interrupted by an unexpected wake command"
    );

    cancel.cancel();
    let _ = handle.await;
}
