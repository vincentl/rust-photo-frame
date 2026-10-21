//! DRM/KMS startup: waiting for a connected display while still answering
//! the control socket.
//!
//! The viewer cannot start without a connected connector (a mode to pick,
//! a Vulkan display to acquire). That wait used to be deaf: commands queued
//! in the channel until a display appeared. Once the viewer switches the
//! panel off (`display-power off`), that is a trap. The viewer restarts while
//! the panel is dark (a deploy, a crash), and a TV in standby that dropped
//! hot-plug detect raises it only when it sees a signal, which nothing
//! sends. Every `display-power on` then timed out, and the frame stayed
//! dark until someone found the TV's remote.
//!
//! So while waiting, the viewer answers `display-power` itself:
//! * **on** forces a probe of every connector (the kernel re-reads hot-plug
//!   detect and the EDID) and, if a display now reads connected, stops
//!   waiting: startup's own modeset lights the panel, and the reply says so.
//!   If none does, the reply says why and the wait goes on.
//! * **off** and a **query** answer "off": nothing is lit.
//!
//! Every other command (set-state, the Wi-Fi overlay) is held, in order,
//! and handed to the viewer once it runs; only the newest overlay content is
//! kept, since each one replaces the last.

// Driven by the KMS loop, which only builds on Linux; tested everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::Duration;

use config_model::control::{ErrorKind, PowerState, Reply};
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

use crate::drm_display::{KmsError, KmsErrorKind};
use crate::events::ViewerCommand;

/// How the wait ended.
pub(super) enum Waited<T> {
    /// A display is connected: start on it, then apply `held`.
    Ready { target: T, held: Vec<ViewerCommand> },
    /// Shutdown (cancelled, or the control channel closed).
    Cancelled,
}

/// Poll `attempt` every `poll` until it finds a display, answering
/// `display-power` meanwhile (see the module docs). `attempt(true)` forces a
/// connector probe first. `describe` gives the connector and mode for a
/// reply. Errors other than "no connected display" end the wait.
pub(super) async fn wait_answering<T>(
    mut attempt: impl FnMut(bool) -> Result<T, KmsError>,
    describe: impl Fn(&T) -> (String, String),
    poll: Duration,
    cancel: &CancellationToken,
    control: &mut Receiver<ViewerCommand>,
    mut on_wait: impl FnMut(&KmsError),
) -> Result<Waited<T>, KmsError> {
    let mut held = Vec::new();
    loop {
        match attempt(false) {
            Ok(target) => return Ok(Waited::Ready { target, held }),
            Err(err) if err.kind() == KmsErrorKind::NoConnectedDisplay => on_wait(&err),
            Err(err) => return Err(err),
        }
        let sleep = tokio::time::sleep(poll);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(Waited::Cancelled),
                cmd = control.recv() => match cmd {
                    None => return Ok(Waited::Cancelled),
                    Some(ViewerCommand::DisplayPower { state, reply, .. }) => {
                        if state != Some(PowerState::On) {
                            // Off or a query: nothing is lit.
                            let _ = reply.send(Reply {
                                state: Some(PowerState::Off),
                                ..Reply::ok()
                            });
                            continue;
                        }
                        match attempt(true) {
                            Ok(target) => {
                                let (connector, mode) = describe(&target);
                                tracing::info!(%connector, %mode, "kms_waiting_for_display: display-power on found a display after a forced probe");
                                let _ = reply.send(Reply::power(PowerState::On, connector, mode));
                                return Ok(Waited::Ready { target, held });
                            }
                            Err(err) if err.kind() == KmsErrorKind::NoConnectedDisplay => {
                                tracing::warn!(error = %err, "kms_waiting_for_display: display-power on, but no display reads connected even after a forced probe");
                                let _ = reply.send(Reply::error(
                                    ErrorKind::Failed,
                                    format!(
                                        "no display is connected, even after a forced probe ({err}); \
                                         a TV in standby that dropped hot-plug detect has to be woken \
                                         with its own remote or input button"
                                    ),
                                ));
                            }
                            Err(err) => {
                                let _ = reply.send(Reply::error(ErrorKind::Failed, err.to_string()));
                                return Err(err);
                            }
                        }
                    }
                    Some(cmd @ ViewerCommand::ShowWifiOverlay(_)) => {
                        held.retain(|c| !matches!(c, ViewerCommand::ShowWifiOverlay(_)));
                        held.push(cmd);
                    }
                    Some(cmd) => held.push(cmd),
                },
                _ = &mut sleep => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ViewerState;
    use config_model::control::WifiOverlay;
    use std::cell::RefCell;
    use tokio::sync::{mpsc, oneshot};

    const POLL: Duration = Duration::from_secs(1);

    fn none() -> KmsError {
        KmsError::no_connected_display("HDMI-A-1 is Disconnected")
    }

    fn overlay(password: &str) -> WifiOverlay {
        WifiOverlay {
            ttl_sec: 30,
            headline: "h".into(),
            body: "b".into(),
            ssid: "PhotoFrame-Setup".into(),
            password: password.into(),
            join_qr: "WIFI:T:WPA;S:x;P:y;;".into(),
            portal_url: "http://192.168.4.1:8080/".into(),
            footer: "f".into(),
        }
    }

    fn power(state: Option<PowerState>) -> (ViewerCommand, oneshot::Receiver<Reply>) {
        let (reply, rx) = oneshot::channel();
        (
            ViewerCommand::DisplayPower {
                state,
                output: None,
                reply,
            },
            rx,
        )
    }

    fn describe(target: &&'static str) -> (String, String) {
        (target.to_string(), "3840x2160@60".to_string())
    }

    /// The display shows up on its own; commands that arrived meanwhile
    /// are held in order, with only the newest overlay content.
    #[tokio::test(start_paused = true)]
    async fn holds_commands_until_a_display_appears() {
        let (tx, mut rx) = mpsc::channel(16);
        tx.send(ViewerCommand::SetState(ViewerState::Asleep))
            .await
            .unwrap();
        tx.send(ViewerCommand::ShowWifiOverlay(Box::new(overlay("old"))))
            .await
            .unwrap();
        tx.send(ViewerCommand::ToggleState).await.unwrap();
        tx.send(ViewerCommand::ShowWifiOverlay(Box::new(overlay("new"))))
            .await
            .unwrap();
        let polls = RefCell::new(0);
        let waited = wait_answering(
            |_force| {
                *polls.borrow_mut() += 1;
                if *polls.borrow() < 3 {
                    Err(none())
                } else {
                    Ok("HDMI-A-1")
                }
            },
            describe,
            POLL,
            &CancellationToken::new(),
            &mut rx,
            |_| {},
        )
        .await
        .unwrap();
        let Waited::Ready { target, held } = waited else {
            panic!("cancelled");
        };
        assert_eq!(target, "HDMI-A-1");
        assert!(matches!(
            held[0],
            ViewerCommand::SetState(ViewerState::Asleep)
        ));
        assert!(matches!(held[1], ViewerCommand::ToggleState));
        match &held[2] {
            ViewerCommand::ShowWifiOverlay(o) => assert_eq!(o.password, "new"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(held.len(), 3);
    }

    /// Review N2: a wake while waiting forces a probe and, when that finds
    /// the display, starts on it at once and says so.
    #[tokio::test(start_paused = true)]
    async fn display_power_on_forces_a_probe_and_starts() {
        let (tx, mut rx) = mpsc::channel(16);
        let (cmd, reply) = power(Some(PowerState::On));
        tx.send(cmd).await.unwrap();
        let forced = RefCell::new(Vec::new());
        let waited = wait_answering(
            |force| {
                forced.borrow_mut().push(force);
                if force { Ok("HDMI-A-2") } else { Err(none()) }
            },
            describe,
            POLL,
            &CancellationToken::new(),
            &mut rx,
            |_| {},
        )
        .await
        .unwrap();
        assert!(matches!(
            waited,
            Waited::Ready {
                target: "HDMI-A-2",
                ..
            }
        ));
        assert_eq!(*forced.borrow(), vec![false, true]);
        let reply = reply.await.unwrap();
        assert_eq!(
            reply,
            Reply::power(PowerState::On, "HDMI-A-2", "3840x2160@60")
        );
    }

    /// When even a forced probe finds nothing, the wake is answered with
    /// the reason at once (no timeout) and the wait goes on.
    #[tokio::test(start_paused = true)]
    async fn a_wake_with_no_display_is_answered_and_the_wait_goes_on() {
        let (tx, mut rx) = mpsc::channel(16);
        let (cmd, reply) = power(Some(PowerState::On));
        tx.send(cmd).await.unwrap();
        let (query, query_reply) = power(None);
        tx.send(query).await.unwrap();
        let (off, off_reply) = power(Some(PowerState::Off));
        tx.send(off).await.unwrap();
        let polls = RefCell::new(0);
        let waited = wait_answering(
            |force| {
                if force {
                    return Err(none());
                }
                *polls.borrow_mut() += 1;
                if *polls.borrow() < 4 {
                    Err(none())
                } else {
                    Ok("HDMI-A-1")
                }
            },
            describe,
            POLL,
            &CancellationToken::new(),
            &mut rx,
            |_| {},
        )
        .await
        .unwrap();
        assert!(matches!(waited, Waited::Ready { .. }));
        let reply = reply.await.unwrap();
        assert_eq!(reply.error, Some(ErrorKind::Failed));
        assert!(reply.detail.unwrap().contains("remote"));
        for r in [query_reply.await.unwrap(), off_reply.await.unwrap()] {
            assert!(r.ok);
            assert_eq!(r.state, Some(PowerState::Off));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn other_errors_end_the_wait() {
        let (_tx, mut rx) = mpsc::channel::<ViewerCommand>(1);
        let result = wait_answering(
            |_| Err::<&str, _>(KmsError::failed("no DRM card")),
            describe,
            POLL,
            &CancellationToken::new(),
            &mut rx,
            |_| {},
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_ends_the_wait() {
        let (_tx, mut rx) = mpsc::channel::<ViewerCommand>(1);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let waited = wait_answering(
            |_| Err::<&str, _>(none()),
            describe,
            POLL,
            &cancel,
            &mut rx,
            |_| {},
        )
        .await
        .unwrap();
        assert!(matches!(waited, Waited::Cancelled));
    }
}
