//! What the panel shows, as a pure function of three independent inputs:
//!
//! * the viewer **mode** (Greeting, Wake, Sleep), switched by `set-state`;
//! * the **Wi-Fi overlay lease**, set by `wifi-overlay-show` and gone on
//!   `wifi-overlay-hide` or when its TTL runs out without a refresh;
//! * **panel power** on DRM/KMS (`display-power`); always lit on Wayland,
//!   where the compositor owns the output.
//!
//! ```text
//! layer = Nothing        while the panel is dark
//!       = Overlay        while a lease is live, except in Sleep
//!       = Scene(mode)    otherwise
//! ```
//!
//! The overlay is held, not drawn, during Sleep (an explicit request for a
//! quiet frame) and while the panel is dark; it appears when the frame wakes
//! or the panel lights. The slideshow advances only while it is the layer on
//! screen (`Scene(Wake)`): a photo nobody can see is not reported as
//! displayed, because `Displayed` feeds the scheduler's spread. See
//! `WakeScene::set_visible` for what hiding and revealing it do.
//!
//! Panel power is its own small state machine, [`PanelPower`]; `kms.rs`
//! documents how it drives the display.

// Panel power changes only on DRM/KMS, which builds only on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::{Duration, Instant};

use config_model::control::{PowerState, WifiOverlay};

use super::ViewerModeKind;

/// What the next frame draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layer {
    /// The panel is dark: draw nothing.
    Nothing,
    /// The Wi-Fi recovery overlay, full screen.
    Overlay,
    /// The active scene.
    Scene(ViewerModeKind),
}

/// What a `wifi-overlay-show` changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OverlayShown {
    /// New or different content: the overlay must be (re)rendered.
    Content,
    /// The same content again: only the lease moved.
    Refreshed,
}

struct OverlayLease {
    content: WifiOverlay,
    expires_at: Instant,
}

/// The overlay lease and the panel's power, plus the layer they select.
pub(super) struct Presentation {
    overlay: Option<OverlayLease>,
    power: PanelPower,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            overlay: None,
            power: PanelPower::Lit { since: None },
        }
    }
}

impl Presentation {
    pub(super) fn layer(&self, mode: ViewerModeKind, now: Instant) -> Layer {
        if self.power.is_dark() {
            Layer::Nothing
        } else if self.overlay_live(now) && mode != ViewerModeKind::Sleep {
            Layer::Overlay
        } else {
            Layer::Scene(mode)
        }
    }

    fn overlay_live(&self, now: Instant) -> bool {
        self.overlay.as_ref().is_some_and(|o| now < o.expires_at)
    }

    /// Set or extend the lease to `now + ttl`.
    pub(super) fn show_overlay(&mut self, content: WifiOverlay, now: Instant) -> OverlayShown {
        let expires_at = now + content.ttl();
        match &mut self.overlay {
            Some(lease) if lease.content.same_content(&content) => {
                lease.expires_at = expires_at;
                OverlayShown::Refreshed
            }
            slot => {
                *slot = Some(OverlayLease {
                    content,
                    expires_at,
                });
                OverlayShown::Content
            }
        }
    }

    /// Remove the overlay; whether there was one.
    pub(super) fn hide_overlay(&mut self) -> bool {
        self.overlay.take().is_some()
    }

    /// Drop a lease whose TTL has run out; whether one did.
    pub(super) fn expire_overlay(&mut self, now: Instant) -> bool {
        if self.overlay.is_some() && !self.overlay_live(now) {
            self.overlay = None;
            true
        } else {
            false
        }
    }

    pub(super) fn overlay(&self) -> Option<&WifiOverlay> {
        self.overlay.as_ref().map(|o| &o.content)
    }

    /// When the lease runs out (the loop must wake then to take it down).
    pub(super) fn overlay_expires_at(&self) -> Option<Instant> {
        self.overlay.as_ref().map(|o| o.expires_at)
    }

    pub(super) fn power(&self) -> &PanelPower {
        &self.power
    }

    pub(super) fn power_mut(&mut self) -> &mut PanelPower {
        &mut self.power
    }
}

/// The panel's power on DRM/KMS.
///
/// ```text
/// Lit    --off-->  Dark      DPMS property := OFF (the caller's ioctl)
/// Waking --off-->  Dark      nothing to do: the CRTC never came back on
/// Dark   --on-->   Waking    no ioctl; the next present is the modeset
/// Waking --frame presented-->  Lit { since }
/// ```
///
/// The viewer never sets DPMS ON. While the CRTC is dark, Mesa's page flip
/// fails with EINVAL and its display WSI falls back to `drmModeSetCrtc`, a
/// full modeset, which re-drives the HDMI link and forces DPMS on (see
/// `kms.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelPower {
    /// Showing frames. `since` is when the last wake's modeset frame was
    /// presented; `None` when the panel has been lit since startup.
    Lit { since: Option<Instant> },
    /// DPMS off; nothing is presented.
    Dark,
    /// Switched on, but the modeset frame has not been presented yet.
    Waking,
}

/// What switching the panel off takes from the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OffAction {
    /// Set the connector's DPMS property to OFF, then [`PanelPower::set_dark`].
    DpmsOff,
    /// The CRTC is still dark (switched on, no frame yet): just mark it.
    MarkDark,
    /// Already dark.
    Nothing,
}

impl PanelPower {
    pub(super) fn is_dark(&self) -> bool {
        matches!(self, PanelPower::Dark)
    }

    pub(super) fn off_action(&self) -> OffAction {
        match self {
            PanelPower::Lit { .. } => OffAction::DpmsOff,
            PanelPower::Waking => OffAction::MarkDark,
            PanelPower::Dark => OffAction::Nothing,
        }
    }

    pub(super) fn set_dark(&mut self) {
        *self = PanelPower::Dark;
    }

    /// Switch on; true when the panel was dark (the caller must arrange a
    /// frame: that frame is the modeset).
    pub(super) fn on(&mut self) -> bool {
        if self.is_dark() {
            *self = PanelPower::Waking;
            true
        } else {
            false
        }
    }

    /// A frame was presented; true when it was the modeset frame of a wake.
    pub(super) fn presented(&mut self, now: Instant) -> bool {
        if matches!(self, PanelPower::Waking) {
            *self = PanelPower::Lit { since: Some(now) };
            true
        } else {
            false
        }
    }

    /// Frames may be presented (anything but dark: a present while dark
    /// would light the panel through Mesa's modeset fallback).
    pub(super) fn may_present(&self) -> bool {
        !self.is_dark()
    }

    /// Frames may wait for a vblank: only while lit. A vblank wait on a dark
    /// CRTC fails at once, and two failures disable pacing for the process.
    pub(super) fn may_pace(&self) -> bool {
        matches!(self, PanelPower::Lit { .. })
    }

    /// The connector check may run: lit, and at least `settle` after a
    /// wake's modeset frame. A sleeping sink may drop hot-plug detect and
    /// take seconds to raise it again; until then "asleep" and "unplugged"
    /// look the same.
    pub(super) fn may_check_connector(&self, now: Instant, settle: Duration) -> bool {
        match self {
            PanelPower::Lit { since: None } => true,
            PanelPower::Lit { since: Some(at) } => now >= *at + settle,
            PanelPower::Dark | PanelPower::Waking => false,
        }
    }

    /// What `display-power` reports: the viewer's own state, never the
    /// property read back (the kernel documents that as unreliable).
    pub(super) fn reported(&self) -> PowerState {
        if self.is_dark() {
            PowerState::Off
        } else {
            PowerState::On
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ViewerModeKind::{Greeting, Sleep, Wake};

    fn overlay(ttl_sec: u64) -> WifiOverlay {
        WifiOverlay {
            ttl_sec,
            headline: "Reconnect".into(),
            body: "Use another device.".into(),
            ssid: "PhotoFrame-Setup".into(),
            password: "apple-river-stone".into(),
            join_qr: "WIFI:T:WPA;S:PhotoFrame-Setup;P:apple-river-stone;;".into(),
            portal_url: "http://192.168.4.1:8080/".into(),
            footer: "Resumes automatically.".into(),
        }
    }

    const TTL: Duration = Duration::from_secs(30);

    #[test]
    fn without_an_overlay_the_scene_is_on_screen() {
        let p = Presentation::default();
        let now = Instant::now();
        for mode in [Greeting, Wake, Sleep] {
            assert_eq!(p.layer(mode, now), Layer::Scene(mode));
        }
    }

    #[test]
    fn the_overlay_covers_greeting_and_wake_but_waits_out_sleep() {
        let mut p = Presentation::default();
        let now = Instant::now();
        assert_eq!(p.show_overlay(overlay(30), now), OverlayShown::Content);
        assert_eq!(p.layer(Greeting, now), Layer::Overlay);
        assert_eq!(p.layer(Wake, now), Layer::Overlay);
        // Held during sleep: it appears once the frame wakes.
        assert_eq!(p.layer(Sleep, now), Layer::Scene(Sleep));
        assert_eq!(p.layer(Wake, now + Duration::from_secs(1)), Layer::Overlay);
    }

    #[test]
    fn hide_restores_the_scene() {
        let mut p = Presentation::default();
        let now = Instant::now();
        p.show_overlay(overlay(30), now);
        assert!(p.hide_overlay());
        assert!(!p.hide_overlay(), "a second hide changes nothing");
        assert_eq!(p.layer(Wake, now), Layer::Scene(Wake));
    }

    #[test]
    fn the_lease_runs_out_unless_refreshed() {
        let mut p = Presentation::default();
        let t0 = Instant::now();
        p.show_overlay(overlay(30), t0);
        assert_eq!(p.overlay_expires_at(), Some(t0 + TTL));
        // A refresh with the same content only moves the lease.
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(p.show_overlay(overlay(30), t1), OverlayShown::Refreshed);
        assert_eq!(p.overlay_expires_at(), Some(t1 + TTL));
        assert!(
            !p.expire_overlay(t0 + TTL),
            "still inside the refreshed lease"
        );
        // Without further refreshes it lapses exactly at the deadline.
        assert_eq!(p.layer(Wake, t1 + TTL), Layer::Scene(Wake));
        assert!(p.expire_overlay(t1 + TTL));
        assert!(p.overlay().is_none());
        assert_eq!(p.overlay_expires_at(), None);
    }

    #[test]
    fn new_content_or_a_new_lease_length_are_told_apart() {
        let mut p = Presentation::default();
        let now = Instant::now();
        p.show_overlay(overlay(30), now);
        // A longer lease with the same content is still just a refresh.
        assert_eq!(p.show_overlay(overlay(60), now), OverlayShown::Refreshed);
        let mut changed = overlay(30);
        changed.password = "other-three-words".into();
        assert_eq!(p.show_overlay(changed, now), OverlayShown::Content);
        assert_eq!(
            p.overlay().map(|o| o.password.as_str()),
            Some("other-three-words")
        );
    }

    #[test]
    fn a_dark_panel_draws_nothing_and_holds_the_overlay() {
        let mut p = Presentation::default();
        let now = Instant::now();
        p.power_mut().set_dark();
        p.show_overlay(overlay(30), now);
        for mode in [Greeting, Wake, Sleep] {
            assert_eq!(p.layer(mode, now), Layer::Nothing);
        }
        assert!(p.power_mut().on());
        assert_eq!(p.layer(Wake, now), Layer::Overlay, "appears once lit");
    }

    #[test]
    fn power_off_from_each_state() {
        let mut lit = PanelPower::Lit { since: None };
        assert_eq!(lit.off_action(), OffAction::DpmsOff);
        lit.set_dark();
        assert_eq!(lit.off_action(), OffAction::Nothing);
        let mut waking = PanelPower::Dark;
        assert!(waking.on());
        assert_eq!(waking, PanelPower::Waking);
        assert_eq!(waking.off_action(), OffAction::MarkDark);
    }

    #[test]
    fn wake_takes_one_modeset_frame() {
        let t0 = Instant::now();
        let mut p = PanelPower::Lit { since: None };
        assert!(!p.on(), "on while lit is a no-op");
        p.set_dark();
        assert!(!p.may_present() && !p.may_pace());
        assert_eq!(p.reported(), PowerState::Off);
        assert!(p.on());
        assert!(!p.on(), "a second on while waking is a no-op");
        assert_eq!(p.reported(), PowerState::On);
        // The modeset frame is presented, but not paced: its CRTC is dark
        // until that frame's modeset.
        assert!(p.may_present() && !p.may_pace());
        assert!(p.presented(t0));
        assert_eq!(p, PanelPower::Lit { since: Some(t0) });
        assert!(p.may_pace());
        assert!(!p.presented(t0), "later frames are ordinary");
    }

    #[test]
    fn the_connector_check_waits_out_the_settle_window() {
        let settle = Duration::from_secs(15);
        let t0 = Instant::now();
        assert!(PanelPower::Lit { since: None }.may_check_connector(t0, settle));
        assert!(!PanelPower::Dark.may_check_connector(t0, settle));
        assert!(!PanelPower::Waking.may_check_connector(t0, settle));
        let woken = PanelPower::Lit { since: Some(t0) };
        assert!(!woken.may_check_connector(t0 + Duration::from_secs(14), settle));
        assert!(woken.may_check_connector(t0 + settle, settle));
    }
}
