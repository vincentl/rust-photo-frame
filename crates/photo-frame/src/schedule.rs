use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::config::AwakeScheduleConfig;
use crate::events::{ViewerCommand, ViewerState};

pub async fn run(
    schedule: AwakeScheduleConfig,
    cancel: CancellationToken,
    control: mpsc::Sender<ViewerCommand>,
    greeting_delay: Duration,
) -> Result<()> {
    let tz = schedule.timezone();
    let mut last_state: Option<bool> = None;
    let mut initial_awake_deadline: Option<Instant> = None;

    enum NextEvent {
        Idle,
        InitialAwake,
        Transition { at: DateTime<Tz>, awake_after: bool },
    }

    loop {
        let now_local = Utc::now().with_timezone(&tz);
        let awake_now = schedule.is_awake_at(now_local);

        if last_state != Some(awake_now) {
            if awake_now {
                if last_state.is_none() {
                    if greeting_delay.is_zero() {
                        let state = ViewerState::Awake;
                        info!(
                            %now_local,
                            state = ?state,
                            "schedule enforcing viewer state"
                        );
                        control
                            .send(ViewerCommand::SetState(state))
                            .await
                            .context("failed to send scheduled viewer command")?;
                        last_state = Some(true);
                        continue;
                    }
                    if initial_awake_deadline.is_none() {
                        let deadline = Instant::now() + greeting_delay;
                        let delay_ms = greeting_delay.as_millis().min(u128::from(u64::MAX)) as u64;
                        debug!(delay_ms, "schedule deferring initial wake");
                        initial_awake_deadline = Some(deadline);
                    }
                } else {
                    let state = ViewerState::Awake;
                    info!(
                        %now_local,
                        state = ?state,
                        "schedule enforcing viewer state"
                    );
                    control
                        .send(ViewerCommand::SetState(state))
                        .await
                        .context("failed to send scheduled viewer command")?;
                    last_state = Some(true);
                    continue;
                }
            } else {
                initial_awake_deadline = None;
                let state = ViewerState::Asleep;
                info!(
                    %now_local,
                    state = ?state,
                    "schedule enforcing viewer state"
                );
                control
                    .send(ViewerCommand::SetState(state))
                    .await
                    .context("failed to send scheduled viewer command")?;
                last_state = Some(false);
                continue;
            }
        }

        let mut wait = Duration::from_secs(60);
        let mut next_event = NextEvent::Idle;

        if let Some(deadline) = initial_awake_deadline {
            let now_instant = Instant::now();
            let until = deadline
                .checked_duration_since(now_instant)
                .unwrap_or_else(|| Duration::from_secs(0));
            wait = until;
            next_event = NextEvent::InitialAwake;
        }

        if let Some((transition_at, awake_after)) = schedule.next_transition_after(now_local) {
            let now_utc = now_local.with_timezone(&Utc);
            let transition_utc = transition_at.with_timezone(&Utc);
            let delta = transition_utc.signed_duration_since(now_utc);
            let transition_wait = match delta.to_std() {
                Ok(duration) => duration,
                Err(_) => Duration::from_secs(0),
            };

            if matches!(next_event, NextEvent::Idle) || transition_wait < wait {
                wait = transition_wait;
                next_event = NextEvent::Transition {
                    at: transition_at,
                    awake_after,
                };
            }
        }

        match next_event {
            NextEvent::Idle => {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = sleep(wait) => {}
                }
            }
            NextEvent::InitialAwake => {
                if wait.is_zero() {
                    initial_awake_deadline = None;
                    let now_local = Utc::now().with_timezone(&tz);
                    let awake = schedule.is_awake_at(now_local);
                    let state = if awake {
                        ViewerState::Awake
                    } else {
                        ViewerState::Asleep
                    };
                    info!(
                        %now_local,
                        state = ?state,
                        "schedule enforcing viewer state"
                    );
                    control
                        .send(ViewerCommand::SetState(state))
                        .await
                        .context("failed to send scheduled viewer command")?;
                    last_state = Some(awake);
                    continue;
                }

                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = sleep(wait) => {
                        initial_awake_deadline = None;
                        let now_local = Utc::now().with_timezone(&tz);
                        let awake = schedule.is_awake_at(now_local);
                        let state = if awake {
                            ViewerState::Awake
                        } else {
                            ViewerState::Asleep
                        };
                        info!(
                            %now_local,
                            state = ?state,
                            "schedule enforcing viewer state"
                        );
                        control
                            .send(ViewerCommand::SetState(state))
                            .await
                            .context("failed to send scheduled viewer command")?;
                        last_state = Some(awake);
                    }
                }
            }
            NextEvent::Transition { at, awake_after } => {
                let desired_state = if awake_after {
                    ViewerState::Awake
                } else {
                    ViewerState::Asleep
                };

                if wait.is_zero() {
                    debug!(
                        %at,
                        state = ?desired_state,
                        "schedule executing immediate transition"
                    );
                    control
                        .send(ViewerCommand::SetState(desired_state))
                        .await
                        .context("failed to send scheduled viewer command")?;
                    last_state = Some(awake_after);
                    if !awake_after {
                        initial_awake_deadline = None;
                    }
                    continue;
                }

                debug!(
                    %at,
                    state = ?desired_state,
                    wait_secs = wait.as_secs_f64(),
                    "schedule awaiting transition"
                );

                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = sleep(wait) => {
                        control
                            .send(ViewerCommand::SetState(desired_state))
                            .await
                            .context("failed to send scheduled viewer command")?;
                        last_state = Some(awake_after);
                        if !awake_after {
                            initial_awake_deadline = None;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
