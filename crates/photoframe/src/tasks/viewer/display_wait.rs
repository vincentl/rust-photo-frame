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
//! * **off** and a **query** answer "off": nothing is lit. An off is also
//!   remembered: if a display appears later on its own (a TV that turns
//!   itself on, hot-plug detect flapping), the viewer starts with the panel
//!   dark, as asked, instead of lighting it (review D1). A later on clears
//!   it; the last explicit request wins.
//! * A request naming an `output` other than the connector the viewer is
//!   configured to drive (`PHOTOFRAME_DRM_CONNECTOR`) is refused as
//!   `not-driven`, as it is once running, and changes nothing. With no
//!   configured connector the viewer drives the first one that connects,
//!   which is not known while none is, so a named output is accepted.
//!
//! A forced probe of a TV that raises hot-plug detect while its DDC does
//! not answer can spend seconds in the kernel's EDID retries (review D2).
//! If that outlasts the control task's 5 s, the caller gets `timeout`
//! (whose detail says the viewer may still apply it) while the viewer then
//! starts lit. That is the request carried out late, not a wrong state:
//! buttond's next check reads `on` and agrees. Answering before probing
//! would only trade it for a reply that cannot say whether the panel lit.
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
    /// A display is connected: start on it, then apply `held`. With
    /// `start_dark`, the last power request while waiting was off: start
    /// with the panel off.
    Ready {
        target: T,
        held: Vec<ViewerCommand>,
        start_dark: bool,
    },
    /// Shutdown (cancelled, or the control channel closed).
    Cancelled,
}

/// Poll `attempt` every `poll` until it finds a display, answering
/// `display-power` meanwhile (see the module docs). `attempt(true)` forces a
/// connector probe first. `describe` gives the connector and mode for a
/// reply. `driven` is the configured connector, if any. Errors other than
/// "no connected display" end the wait.
#[allow(clippy::too_many_arguments)]
pub(super) async fn wait_answering<T>(
    mut attempt: impl FnMut(bool) -> Result<T, KmsError>,
    describe: impl Fn(&T) -> (String, String),
    driven: Option<&str>,
    poll: Duration,
    cancel: &CancellationToken,
    control: &mut Receiver<ViewerCommand>,
    mut on_wait: impl FnMut(&KmsError),
) -> Result<Waited<T>, KmsError> {
    let mut held = Vec::new();
    let mut start_dark = false;
    loop {
        match attempt(false) {
            Ok(target) => {
                return Ok(Waited::Ready {
                    target,
                    held,
                    start_dark,
                });
            }
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
                    Some(ViewerCommand::DisplayPower { state, output, reply }) => {
                        if let (Some(output), Some(driven)) = (output.as_deref(), driven)
                            && output != "*"
                            && !output.eq_ignore_ascii_case(driven)
                        {
                            let _ = reply.send(Reply::error(
                                ErrorKind::NotDriven,
                                format!("this viewer drives {driven}, not {output}"),
                            ));
                            continue;
                        }
                        if state != Some(PowerState::On) {
                            // Off or a query: nothing is lit. An off is the
                            // intent the viewer starts with.
                            if state == Some(PowerState::Off) {
                                start_dark = true;
                            }
                            let _ = reply.send(Reply {
                                state: Some(PowerState::Off),
                                ..Reply::ok()
                            });
                            continue;
                        }
                        start_dark = false;
                        match attempt(true) {
                            Ok(target) => {
                                let (connector, mode) = describe(&target);
                                tracing::info!(%connector, %mode, "kms_waiting_for_display: display-power on found a display after a forced probe");
                                let _ = reply.send(Reply::power(PowerState::On, connector, mode));
                                return Ok(Waited::Ready {
                                    target,
                                    held,
                                    start_dark: false,
                                });
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
            None,
            POLL,
            &CancellationToken::new(),
            &mut rx,
            |_| {},
        )
        .await
        .unwrap();
        let Waited::Ready {
            target,
            held,
            start_dark,
        } = waited
        else {
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
        assert!(!start_dark, "no power request: start lit as usual");
    }

    /// Review D1: an off while waiting is remembered. When the display
    /// appears on its own later, the viewer starts dark, as asked; a later
    /// on (even one whose probe finds nothing) replaces it, and a query
    /// changes nothing.
    #[tokio::test(start_paused = true)]
    async fn the_last_power_request_while_waiting_decides_how_to_start() {
        // (requests, expected start_dark)
        let cases: [(&[Option<PowerState>], bool); 4] = [
            (&[Some(PowerState::Off)], true),
            (&[Some(PowerState::Off), None], true),
            (&[Some(PowerState::Off), Some(PowerState::On)], false),
            (&[Some(PowerState::On), Some(PowerState::Off)], true),
        ];
        for (requests, expected) in cases {
            let (tx, mut rx) = mpsc::channel(16);
            let mut replies = Vec::new();
            for state in requests {
                let (cmd, reply) = power(*state);
                tx.send(cmd).await.unwrap();
                replies.push(reply);
            }
            let polls = RefCell::new(0);
            let waited = wait_answering(
                |force| {
                    if force {
                        return Err(none()); // the forced probe finds nothing
                    }
                    *polls.borrow_mut() += 1;
                    if *polls.borrow() < 3 {
                        Err(none())
                    } else {
                        Ok("HDMI-A-1")
                    }
                },
                describe,
                None,
                POLL,
                &CancellationToken::new(),
                &mut rx,
                |_| {},
            )
            .await
            .unwrap();
            let Waited::Ready { start_dark, .. } = waited else {
                panic!("cancelled");
            };
            assert_eq!(start_dark, expected, "{requests:?}");
            for reply in replies {
                reply.await.expect("every request is answered");
            }
        }
    }

    /// Review D2: a request naming another output than the configured one
    /// is refused and is not an intent; a matching or wildcard one is
    /// handled.
    #[tokio::test(start_paused = true)]
    async fn a_request_for_another_output_is_not_driven() {
        let (tx, mut rx) = mpsc::channel(16);
        let named = |state, output: &str| {
            let (reply, rx) = oneshot::channel();
            (
                ViewerCommand::DisplayPower {
                    state: Some(state),
                    output: Some(output.to_string()),
                    reply,
                },
                rx,
            )
        };
        let (other_off, other_off_reply) = named(PowerState::Off, "HDMI-A-1");
        let (other_on, other_on_reply) = named(PowerState::On, "HDMI-A-1");
        tx.send(other_off).await.unwrap();
        tx.send(other_on).await.unwrap();
        let forced = RefCell::new(0);
        let polls = RefCell::new(0);
        let waited = wait_answering(
            |force| {
                if force {
                    *forced.borrow_mut() += 1;
                    return Ok("HDMI-A-2");
                }
                *polls.borrow_mut() += 1;
                if *polls.borrow() < 3 {
                    Err(none())
                } else {
                    Ok("HDMI-A-2")
                }
            },
            describe,
            Some("HDMI-A-2"),
            POLL,
            &CancellationToken::new(),
            &mut rx,
            |_| {},
        )
        .await
        .unwrap();
        for reply in [
            other_off_reply.await.unwrap(),
            other_on_reply.await.unwrap(),
        ] {
            assert_eq!(reply.error, Some(ErrorKind::NotDriven));
        }
        assert_eq!(*forced.borrow(), 0, "a refused request probes nothing");
        let Waited::Ready { start_dark, .. } = waited else {
            panic!("cancelled");
        };
        assert!(!start_dark, "a refused off is not an intent");
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
            None,
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
            None,
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
            None,
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
            None,
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
