//! The viewer's control-socket protocol: the one definition of the wire
//! types, shared by the viewer (which reads requests and writes replies) and
//! its clients (`wifi-manager`; `powerctl` and `buttond` speak the same JSON
//! from the shell and by hand).
//!
//! **Framing.** One JSON object per connection. The client writes the
//! request and half-closes its write side; the viewer then writes one JSON
//! [`Reply`] line and closes. Reading the reply is optional: a client that
//! writes and drops the connection (buttond) still has its command applied.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where the viewer listens unless `control-socket-path` says otherwise.
pub const DEFAULT_SOCKET_PATH: &str = "/run/photoframe/control.sock";

/// The longest text field the viewer accepts in a [`WifiOverlay`], in chars.
pub const MAX_OVERLAY_TEXT_CHARS: usize = 256;
/// The longest QR payload the viewer accepts, in bytes.
pub const MAX_QR_PAYLOAD_BYTES: usize = 512;
/// The overlay lease the viewer applies when a request carries none.
pub const DEFAULT_OVERLAY_TTL_SEC: u64 = 30;
/// Bounds on the overlay lease: short enough that a dead sender's overlay
/// goes away, long enough that a refresh every 10 s keeps it up.
pub const MIN_OVERLAY_TTL_SEC: u64 = 5;
pub const MAX_OVERLAY_TTL_SEC: u64 = 300;

/// One request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Request {
    /// `{"command":"set-state","state":"awake"|"asleep"}`
    SetState { state: ViewerState },
    /// `{"command":"toggle-state"}`
    ToggleState,
    /// `{"command":"display-power","state":"on"|"off","output":"HDMI-A-2"}`;
    /// without `state` it is a query. `output`, when given, must name the
    /// connector the viewer drives.
    DisplayPower {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<PowerState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Show (or refresh) the Wi-Fi recovery overlay for `ttl-sec`.
    WifiOverlayShow(WifiOverlay),
    /// Remove the Wi-Fi recovery overlay.
    WifiOverlayHide,
    /// `{"command":"ping"}`: answered `ok` and nothing else. A client checks
    /// with it that the viewer speaks this protocol before sending anything
    /// sensitive: a viewer from before replies existed answers nothing, and
    /// logs a payload it cannot parse.
    Ping,
}

impl Request {
    /// The command name, for logs that must not carry the payload.
    pub fn name(&self) -> &'static str {
        match self {
            Request::SetState { .. } => "set-state",
            Request::ToggleState => "toggle-state",
            Request::DisplayPower { .. } => "display-power",
            Request::WifiOverlayShow(_) => "wifi-overlay-show",
            Request::WifiOverlayHide => "wifi-overlay-hide",
            Request::Ping => "ping",
        }
    }
}

/// The `command` field of a payload that may not parse as a [`Request`],
/// so a parse failure can be logged without the rest of the payload (an
/// overlay request carries the hotspot password).
pub fn command_name(payload: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Head {
        command: String,
    }
    serde_json::from_slice::<Head>(payload)
        .ok()
        .map(|head| head.command.chars().take(64).collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewerState {
    Awake,
    Asleep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    On,
    Off,
}

impl PowerState {
    pub fn as_str(self) -> &'static str {
        match self {
            PowerState::On => "on",
            PowerState::Off => "off",
        }
    }
}

/// What the Wi-Fi recovery overlay shows. The sender owns the prose
/// (headline, body, footer); the viewer owns the layout and the step labels.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WifiOverlay {
    /// Lease in seconds; the viewer hides the overlay when it runs out
    /// without a refresh. Clamped to
    /// [`MIN_OVERLAY_TTL_SEC`]..=[`MAX_OVERLAY_TTL_SEC`].
    #[serde(default = "default_ttl_sec")]
    pub ttl_sec: u64,
    pub headline: String,
    pub body: String,
    /// The recovery hotspot's SSID.
    pub ssid: String,
    /// The hotspot's passphrase, shown for manual joining.
    pub password: String,
    /// QR payload that joins the hotspot (`WIFI:T:WPA;S:…;P:…;;`).
    pub join_qr: String,
    /// The setup page; also the second QR code's payload.
    pub portal_url: String,
    pub footer: String,
}

fn default_ttl_sec() -> u64 {
    DEFAULT_OVERLAY_TTL_SEC
}

impl WifiOverlay {
    /// The lease, clamped to the accepted range.
    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl_sec.clamp(MIN_OVERLAY_TTL_SEC, MAX_OVERLAY_TTL_SEC))
    }

    /// Whether the content is within the limits the viewer renders. The
    /// error names the field, never its value.
    pub fn validate(&self) -> Result<(), String> {
        for (name, text) in [
            ("headline", &self.headline),
            ("body", &self.body),
            ("ssid", &self.ssid),
            ("password", &self.password),
            ("footer", &self.footer),
        ] {
            if text.chars().count() > MAX_OVERLAY_TEXT_CHARS {
                return Err(format!(
                    "{name} is longer than {MAX_OVERLAY_TEXT_CHARS} characters"
                ));
            }
        }
        for (name, payload) in [("join-qr", &self.join_qr), ("portal-url", &self.portal_url)] {
            if payload.is_empty() {
                return Err(format!("{name} is empty"));
            }
            if payload.len() > MAX_QR_PAYLOAD_BYTES {
                return Err(format!(
                    "{name} is longer than {MAX_QR_PAYLOAD_BYTES} bytes"
                ));
            }
        }
        Ok(())
    }

    /// Equal content, ignoring the lease (a refresh that changes nothing
    /// on screen).
    pub fn same_content(&self, other: &WifiOverlay) -> bool {
        WifiOverlay {
            ttl_sec: 0,
            ..self.clone()
        } == WifiOverlay {
            ttl_sec: 0,
            ..other.clone()
        }
    }
}

/// The password and the join payload (which embeds it) never reach a log.
impl fmt::Debug for WifiOverlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WifiOverlay")
            .field("ttl_sec", &self.ttl_sec)
            .field("headline", &self.headline)
            .field("ssid", &self.ssid)
            .field("password", &"<redacted>")
            .field("join_qr", &"<redacted>")
            .field("portal_url", &self.portal_url)
            .finish_non_exhaustive()
    }
}

/// Why a request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    /// This viewer cannot do it here (display power under a compositor).
    Unsupported,
    /// `output` names a connector the viewer does not drive.
    NotDriven,
    /// The request was malformed or out of limits.
    Invalid,
    /// The viewer did not answer in time.
    Timeout,
    /// The viewer tried and the operation failed.
    Failed,
}

/// The one reply line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `display-power`: the panel's state as the viewer holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<PowerState>,
    /// `display-power`: the connector the viewer drives, e.g. `HDMI-A-2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// `display-power`: the mode in use, `WxH@Hz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    pub fn power(state: PowerState, connector: impl Into<String>, mode: impl Into<String>) -> Self {
        Self {
            ok: true,
            state: Some(state),
            connector: Some(connector.into()),
            mode: Some(mode.into()),
            ..Self::default()
        }
    }

    pub fn error(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(kind),
            detail: Some(detail.into()),
            ..Self::default()
        }
    }

    /// The reply as sent: one JSON line.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("a reply always serializes");
        line.push('\n');
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay() -> WifiOverlay {
        WifiOverlay {
            ttl_sec: 30,
            headline: "Reconnect to Wi-Fi".into(),
            body: "Use another device to restore connectivity.".into(),
            ssid: "PhotoFrame-Setup".into(),
            password: "apple-river-stone".into(),
            join_qr: "WIFI:T:WPA;S:PhotoFrame-Setup;P:apple-river-stone;;".into(),
            portal_url: "http://192.168.4.1:8080/".into(),
            footer: "The slideshow resumes automatically.".into(),
        }
    }

    fn round_trip(request: &Request) -> Request {
        let json = serde_json::to_string(request).expect("serialize");
        serde_json::from_str(&json).expect("parse back")
    }

    #[test]
    fn every_request_round_trips() {
        for request in [
            Request::SetState {
                state: ViewerState::Awake,
            },
            Request::SetState {
                state: ViewerState::Asleep,
            },
            Request::ToggleState,
            Request::DisplayPower {
                state: Some(PowerState::Off),
                output: Some("HDMI-A-2".into()),
            },
            Request::DisplayPower {
                state: None,
                output: None,
            },
            Request::WifiOverlayShow(overlay()),
            Request::WifiOverlayHide,
            Request::Ping,
        ] {
            assert_eq!(round_trip(&request), request);
        }
    }

    #[test]
    fn wire_names_are_the_documented_ones() {
        let parse = |s: &str| serde_json::from_str::<Request>(s);
        assert_eq!(
            parse(r#"{"command":"set-state","state":"asleep"}"#).unwrap(),
            Request::SetState {
                state: ViewerState::Asleep
            }
        );
        assert_eq!(
            parse(r#"{"command":"display-power","state":"on"}"#).unwrap(),
            Request::DisplayPower {
                state: Some(PowerState::On),
                output: None
            }
        );
        assert_eq!(
            parse(r#"{"command":"display-power"}"#).unwrap(),
            Request::DisplayPower {
                state: None,
                output: None
            }
        );
        assert_eq!(
            parse(r#"{"command":"wifi-overlay-hide"}"#).unwrap(),
            Request::WifiOverlayHide
        );
        let show = serde_json::to_value(Request::WifiOverlayShow(overlay())).unwrap();
        for key in ["command", "ttl-sec", "join-qr", "portal-url", "headline"] {
            assert!(show.get(key).is_some(), "missing {key} in {show}");
        }
        assert_eq!(show["command"], "wifi-overlay-show");
    }

    #[test]
    fn unknown_commands_and_states_are_rejected() {
        let parse = |s: &str| serde_json::from_str::<Request>(s);
        assert!(parse(r#"{"command":"ToggleSleep"}"#).is_err());
        assert!(parse(r#"{"command":"set-state","state":"sleep"}"#).is_err());
        assert!(parse(r#"{"command":"display-power","state":"standby"}"#).is_err());
        assert!(parse(r#"{"command":"wifi-overlay-show","ssid":"x"}"#).is_err());
    }

    #[test]
    fn overlay_ttl_defaults_and_clamps() {
        let json = serde_json::to_value(overlay()).unwrap();
        let mut no_ttl = json.clone();
        no_ttl.as_object_mut().unwrap().remove("ttl-sec");
        let parsed: WifiOverlay = serde_json::from_value(no_ttl).unwrap();
        assert_eq!(parsed.ttl(), Duration::from_secs(DEFAULT_OVERLAY_TTL_SEC));
        let mut o = overlay();
        o.ttl_sec = 0;
        assert_eq!(o.ttl(), Duration::from_secs(MIN_OVERLAY_TTL_SEC));
        o.ttl_sec = 1_000_000;
        assert_eq!(o.ttl(), Duration::from_secs(MAX_OVERLAY_TTL_SEC));
    }

    #[test]
    fn overlay_limits_name_the_field_not_the_value() {
        assert!(overlay().validate().is_ok());
        let mut o = overlay();
        o.password = "p".repeat(MAX_OVERLAY_TEXT_CHARS + 1);
        let err = o.validate().unwrap_err();
        assert!(err.contains("password") && !err.contains("ppp"), "{err}");
        let mut o = overlay();
        o.join_qr.clear();
        assert!(o.validate().unwrap_err().contains("join-qr"));
        let mut o = overlay();
        o.portal_url = "u".repeat(MAX_QR_PAYLOAD_BYTES + 1);
        assert!(o.validate().unwrap_err().contains("portal-url"));
    }

    #[test]
    fn debug_never_shows_the_password() {
        let text = format!("{:?}", Request::WifiOverlayShow(overlay()));
        assert!(!text.contains("apple-river-stone"), "{text}");
        assert!(text.contains("PhotoFrame-Setup"));
    }

    #[test]
    fn same_content_ignores_the_lease() {
        let mut refreshed = overlay();
        refreshed.ttl_sec = 60;
        assert!(overlay().same_content(&refreshed));
        refreshed.password = "other-words-here".into();
        assert!(!overlay().same_content(&refreshed));
    }

    #[test]
    fn command_name_reads_only_the_head() {
        assert_eq!(
            command_name(br#"{"command":"wifi-overlay-show","password":42}"#).as_deref(),
            Some("wifi-overlay-show")
        );
        assert_eq!(command_name(b"not json"), None);
    }

    #[test]
    fn replies_round_trip_and_omit_empty_fields() {
        assert_eq!(Reply::ok().to_line(), "{\"ok\":true}\n");
        for reply in [
            Reply::ok(),
            Reply::power(PowerState::Off, "HDMI-A-2", "3840x2160@60"),
            Reply::error(ErrorKind::NotDriven, "drives HDMI-A-1"),
            Reply::error(ErrorKind::Unsupported, "compositor"),
            Reply::error(ErrorKind::Timeout, "3 s"),
        ] {
            let back: Reply = serde_json::from_str(reply.to_line().trim_end()).unwrap();
            assert_eq!(back, reply);
        }
        let line = Reply::error(ErrorKind::NotDriven, "x").to_line();
        assert!(line.contains("\"error\":\"not-driven\""), "{line}");
    }
}
