use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::config::AwakeScheduleConfig;
use crate::events::{ViewerCommand, ViewerState};

pub async fn run(
    schedule: AwakeScheduleConfig,
    cancel: CancellationToken,
    control: mpsc::Sender<ViewerCommand>,
) -> Result<()> {
    let tz = schedule.timezone();
    let mut last_state: Option<bool> = None;

    loop {
        let now_local = Utc::now().with_timezone(&tz);
        let awake_now = schedule.is_awake_at(now_local);
        if last_state != Some(awake_now) {
            let state = if awake_now {
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
            last_state = Some(awake_now);
        }

        let Some((transition_at, awake_after)) = schedule.next_transition_after(now_local) else {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(60)) => continue,
            };
        };

        let desired_state = if awake_after {
            ViewerState::Awake
        } else {
            ViewerState::Asleep
        };
        let now_utc = now_local.with_timezone(&Utc);
        let transition_utc = transition_at.with_timezone(&Utc);
        let delta = transition_utc.signed_duration_since(now_utc);
        let wait = match delta.to_std() {
            Ok(duration) => duration,
            Err(_) => Duration::from_secs(0),
        };

        if wait.is_zero() {
            debug!(
                %transition_at,
                state = ?desired_state,
                "schedule executing immediate transition"
            );
            control
                .send(ViewerCommand::SetState(desired_state))
                .await
                .context("failed to send scheduled viewer command")?;
            last_state = Some(awake_after);
            continue;
        }

        debug!(
            %transition_at,
            state = ?desired_state,
            wait_secs = wait.as_secs_f64(),
            "schedule awaiting transition"
        );

        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(wait) => {
                control
                    .send(ViewerCommand::SetState(desired_state))
                    .await
                    .context("failed to send scheduled viewer command")?;
                last_state = Some(awake_after);
            }
        }
    }

    Ok(())
}
