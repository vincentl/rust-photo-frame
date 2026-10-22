//! DRM/KMS display target selection, shared by the viewer
//! (`PHOTOFRAME_SURFACE=drm`) and `frametest --drm`.
//!
//! The viewer presents through Vulkan's `VK_KHR_display` +
//! `VK_EXT_acquire_drm_display` (wgpu's `SurfaceTargetUnsafe::Drm`), with no
//! compositor in the loop. Two ordering rules follow from how Mesa's display
//! WSI (`src/vulkan/wsi/wsi_common_display.c`) treats the card fd, and every
//! caller must keep both:
//!
//! 1. **Open the card before the wgpu `Instance` exists.** The kernel grants
//!    implicit DRM master to the first client that opens a primary node while
//!    no master is held. v3dv opens the display node itself at instance
//!    creation and, if that open is master, adopts it as its display fd;
//!    `vkAcquireDrmDisplayEXT` then refuses ("no support for multiple leases")
//!    because the WSI already holds an fd. Opening first makes this process's
//!    fd the master one, and the acquire succeeds.
//! 2. **Keep the card open until after the `Instance` is dropped.**
//!    `vkAcquireDrmDisplayEXT` stores our fd as-is (`wsi->fd = drmFd`, no
//!    dup); only `vkReleaseDisplayEXT` would close it, and wgpu-hal never
//!    calls that. Mesa's vblank wait thread polls the fd until
//!    `wsi_display_finish_wsi` runs at instance destruction. Closing the card
//!    first would leave Mesa polling a closed (or, worse, reused) fd number.
//!    So teardown is always: GPU objects (swapchain) → `Instance` → card.
//!
//! The selection rules (which connector, mode, refresh and plane) are plain
//! functions over plain data so they are unit-tested on every platform; the
//! device layer that feeds them is Linux-only.

use std::fmt;
use std::str::FromStr;

/// A requested display mode, `WxH[@Hz]` (e.g. `3840x2160@60`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeRequest {
    pub width: u32,
    pub height: u32,
    /// `None` accepts any refresh rate at that size.
    pub refresh_hz: Option<u32>,
}

impl FromStr for ModeRequest {
    type Err = KmsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || KmsError::failed(format!("invalid mode {s:?}; expected WxH[@Hz]"));
        let (size, hz) = match s.trim().split_once('@') {
            Some((size, hz)) => (size, Some(hz.parse().map_err(|_| bad())?)),
            None => (s.trim(), None),
        };
        let (w, h) = size.split_once(['x', 'X']).ok_or_else(bad)?;
        let width: u32 = w.parse().map_err(|_| bad())?;
        let height: u32 = h.parse().map_err(|_| bad())?;
        if width == 0 || height == 0 {
            return Err(bad());
        }
        Ok(Self {
            width,
            height,
            refresh_hz: hz,
        })
    }
}

impl fmt::Display for ModeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)?;
        if let Some(hz) = self.refresh_hz {
            write!(f, "@{hz}")?;
        }
        Ok(())
    }
}

/// Error from DRM target selection or DRM surface setup. The message is
/// written for the operator reading the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KmsError {
    kind: KmsErrorKind,
    message: String,
}

/// Whether waiting can fix the error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KmsErrorKind {
    /// A card with KMS resources exists but no (requested) connector is
    /// connected: the panel is off, unplugged, or not passing hot-plug
    /// detect yet. Plugging it in fixes this; see [`wait_for_display`].
    NoConnectedDisplay,
    /// Anything else (no DRM card, not master, Vulkan refused, no such mode).
    Failed,
}

impl KmsError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: KmsErrorKind::Failed,
            message: message.into(),
        }
    }

    pub fn no_connected_display(message: impl Into<String>) -> Self {
        Self {
            kind: KmsErrorKind::NoConnectedDisplay,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> KmsErrorKind {
        self.kind
    }

    /// Same kind, message extended with where it happened.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // used by the Linux device layer
    fn context(self, context: impl fmt::Display) -> Self {
        Self {
            kind: self.kind,
            message: format!("{} ({context})", self.message),
        }
    }
}

impl fmt::Display for KmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KmsError {}

/// One connector as KMS reports it.
#[derive(Clone, Debug)]
pub struct ConnectorCandidate {
    /// e.g. `HDMI-A-1`.
    pub name: String,
    pub connected: bool,
}

/// Choose a connector on one card.
///
/// * `Ok(Some(i))`: use `candidates[i]`.
/// * `Ok(None)`: nothing suitable on this card; try the next one.
/// * `Err`: the operator named a connector that exists but is not connected;
///   that is a hard failure, not a reason to fall through to another card.
///
/// With no name, the first connected connector wins.
pub fn pick_connector(
    candidates: &[ConnectorCandidate],
    name: Option<&str>,
) -> Result<Option<usize>, KmsError> {
    let found = candidates.iter().position(|c| match name {
        Some(n) => n.eq_ignore_ascii_case(&c.name),
        None => c.connected,
    });
    match found {
        Some(i) if !candidates[i].connected => Err(KmsError::no_connected_display(format!(
            "connector {} is not connected",
            candidates[i].name
        ))),
        other => Ok(other),
    }
}

/// The exact time between two vblanks of a progressive mode: one frame is
/// `htotal x vtotal` pixel clocks, and KMS reports the clock in kHz. (The
/// integer `vrefresh` rounds 59.94 and 60 Hz to the same 60.)
pub fn refresh_period(clock_khz: u32, htotal: u16, vtotal: u16) -> std::time::Duration {
    let pixels = u64::from(htotal) * u64::from(vtotal);
    let clock_hz = u64::from(clock_khz.max(1)) * 1000;
    std::time::Duration::from_nanos(pixels * 1_000_000_000 / clock_hz)
}

/// One mode from a connector's KMS mode list.
#[derive(Clone, Copy, Debug)]
pub struct ModeCandidate {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub preferred: bool,
}

fn mode_matches(m: &ModeCandidate, req: ModeRequest) -> bool {
    m.width == req.width
        && m.height == req.height
        && req.refresh_hz.is_none_or(|hz| m.refresh_hz == hz)
}

/// Choose a mode, in this order:
///
/// 1. `request` (an operator's explicit choice) must match exactly (any
///    refresh when the request has none), or it is an error;
/// 2. else `prefer`, if the connector lists it (the viewer passes the mode
///    the sway kiosk config pins, so both stacks drive the panel the same
///    way);
/// 3. else the connector's EDID-preferred mode, else its first mode.
///
/// Among several modes that match a request or preference (e.g. 60 and
/// 59.94 Hz, both reported as 60 by KMS), the EDID-preferred one wins, then
/// the first listed.
pub fn pick_mode(
    modes: &[ModeCandidate],
    request: Option<ModeRequest>,
    prefer: Option<ModeRequest>,
) -> Result<usize, KmsError> {
    if modes.is_empty() {
        return Err(KmsError::failed("connector reports no modes"));
    }
    let best_match = |req: ModeRequest| {
        modes
            .iter()
            .position(|m| m.preferred && mode_matches(m, req))
            .or_else(|| modes.iter().position(|m| mode_matches(m, req)))
    };
    if let Some(req) = request {
        return best_match(req)
            .ok_or_else(|| KmsError::failed(format!("connector has no mode {req}")));
    }
    if let Some(index) = prefer.and_then(best_match) {
        return Ok(index);
    }
    Ok(modes.iter().position(|m| m.preferred).unwrap_or(0))
}

/// From Vulkan's display-mode list (`(width, height, refresh in mHz)`), the
/// refresh value closest to `refresh_hz` among modes of exactly
/// `width`x`height`. The value handed to `SurfaceTargetUnsafe::Drm` must be
/// one Vulkan lists (wgpu-hal compares it exactly), and Vulkan reports
/// e.g. 59940 or 60000 mHz where KMS rounds both to 60.
pub fn closest_refresh_mhz(
    vk_modes: &[(u32, u32, u32)],
    width: u32,
    height: u32,
    refresh_hz: u32,
) -> Option<u32> {
    let want = refresh_hz.saturating_mul(1000);
    vk_modes
        .iter()
        .filter(|(w, h, _)| *w == width && *h == height)
        .map(|(_, _, mhz)| *mhz)
        .min_by_key(|mhz| mhz.abs_diff(want))
}

/// One Vulkan display plane, as it relates to the target display.
#[derive(Clone, Copy, Debug)]
pub struct PlaneCandidate {
    /// The plane currently shows the target display.
    pub current: bool,
    /// The plane can show the target display.
    pub supports: bool,
}

/// The error when no card yielded a target. Waiting only helps when the
/// requested display can still appear: some card has connectors, and a
/// connector named by the operator exists (a misspelled name such as
/// `HDMI-1` for `HDMI-A-1` would otherwise be waited for forever).
pub fn no_target_error(
    notes: &[String],
    saw_kms_card: bool,
    connector_requested: Option<&str>,
    connector_seen: bool,
) -> KmsError {
    let found = format!("no usable DRM card found:\n  {}", notes.join("\n  "));
    match connector_requested {
        Some(name) if !connector_seen => KmsError::failed(format!(
            "no connector is named {name:?} on any card (names look like HDMI-A-1); {found}"
        )),
        _ if saw_kms_card => KmsError::no_connected_display(found),
        _ => KmsError::failed(found),
    }
}

/// Retry `attempt` while it fails only because no display is connected,
/// polling every `poll`; return its value, its first other error, or
/// `Ok(None)` once `cancel` fires.
///
/// The KMS viewer calls this before it creates the wgpu instance, so a frame
/// that boots before its panel (power cut, TV or switch in standby, no HPD
/// yet) waits for the panel the way sway does, instead of exhausting the
/// unit's StartLimit and staying dark. `on_wait` runs once per failed
/// attempt (the caller logs the first).
pub async fn wait_for_display<T>(
    mut attempt: impl FnMut() -> Result<T, KmsError>,
    poll: std::time::Duration,
    cancel: &tokio_util::sync::CancellationToken,
    mut on_wait: impl FnMut(&KmsError),
) -> Result<Option<T>, KmsError> {
    loop {
        match attempt() {
            Ok(value) => return Ok(Some(value)),
            Err(err) if err.kind() == KmsErrorKind::NoConnectedDisplay => {
                on_wait(&err);
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(None),
                    _ = tokio::time::sleep(poll) => {}
                }
            }
            Err(err) => return Err(err),
        }
    }
}

/// The first plane that shows or can show the target display.
pub fn pick_plane(planes: &[PlaneCandidate]) -> Option<u32> {
    planes
        .iter()
        .position(|p| p.current || p.supports)
        .map(|i| i as u32)
}

#[cfg(target_os = "linux")]
mod device {
    use super::*;
    use drm::Device;
    use drm::control::{Device as ControlDevice, ModeTypeFlags, connector};
    use std::fs::{File, OpenOptions};
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
    use std::path::PathBuf;

    /// An open DRM primary node. Dropping it closes the fd, which releases
    /// DRM master; see the module docs for when that may happen.
    pub struct Card(File);

    impl AsFd for Card {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.0.as_fd()
        }
    }
    impl Device for Card {}
    impl ControlDevice for Card {}

    /// What to look for. Every field `None` means: first card with a
    /// connected connector, that connector's preferred mode, and the Vulkan
    /// plane that reports the display.
    #[derive(Clone, Debug, Default)]
    pub struct KmsRequest {
        /// e.g. `/dev/dri/card1`.
        pub card: Option<PathBuf>,
        /// e.g. `HDMI-A-1`.
        pub connector: Option<String>,
        pub mode: Option<ModeRequest>,
        /// Used when `mode` is `None` and the connector lists it; see
        /// [`pick_mode`].
        pub prefer_mode: Option<ModeRequest>,
        /// Vulkan display plane index.
        pub plane: Option<u32>,
        /// Force a probe of each connector (the kernel re-reads hot-plug
        /// detect and the EDID over DDC; tens of ms each) instead of reading
        /// the cached state.
        pub force_probe: bool,
    }

    /// The chosen card, connector and mode. Owns the card fd.
    pub struct KmsTarget {
        pub card: Card,
        pub path: String,
        pub connector_id: u32,
        pub connector_name: String,
        pub width: u32,
        pub height: u32,
        pub vrefresh_hz: u32,
        /// Exact vblank period of the chosen mode ([`refresh_period`]).
        pub refresh_period: std::time::Duration,
        /// Vulkan plane override from the request, if any.
        plane: Option<u32>,
    }

    fn candidate_paths(explicit: Option<&std::path::Path>) -> Vec<String> {
        if let Some(p) = explicit {
            return vec![p.display().to_string()];
        }
        (0..8)
            .map(|i| format!("/dev/dri/card{i}"))
            .filter(|p| std::path::Path::new(p).exists())
            .collect()
    }

    /// Open a card and pick a connector + mode from what KMS reports.
    ///
    /// Call this before creating the wgpu `Instance` (module rule 1).
    /// `trace` receives the enumeration (cards, connectors, modes) as
    /// human-readable lines.
    pub fn open_target(
        request: &KmsRequest,
        trace: &mut dyn FnMut(&str),
    ) -> Result<KmsTarget, KmsError> {
        let mut notes = Vec::new();
        // Whether some card had connectors at all: then "nothing connected"
        // is a display that may still appear, not a missing device.
        let mut saw_kms_card = false;
        // Whether a connector with the requested name exists on any card.
        let mut connector_seen = false;
        for path in candidate_paths(request.card.as_deref()) {
            let file = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(f) => f,
                Err(e) => {
                    notes.push(format!("{path}: open failed: {e}"));
                    continue;
                }
            };
            let card = Card(file);
            let driver = card
                .get_driver()
                .map(|d| d.name().to_string_lossy().into_owned())
                .unwrap_or_else(|_| "?".into());
            let res = match card.resource_handles() {
                Ok(r) => r,
                Err(e) => {
                    // v3d (the render-only GPU node on a Pi 5) has no KMS
                    // resources at all; vc4 is the one with connectors.
                    notes.push(format!("{path} ({driver}): no KMS resources ({e})"));
                    continue;
                }
            };
            saw_kms_card |= !res.connectors().is_empty();
            trace(&format!(
                "{path}: driver={driver} connectors={} crtcs={}",
                res.connectors().len(),
                res.crtcs().len()
            ));
            let mut infos = Vec::new();
            for handle in res.connectors() {
                let Ok(info) = card.get_connector(*handle, request.force_probe) else {
                    continue;
                };
                trace(&format!(
                    "  connector {info} (id {}): {:?}, {} modes",
                    u32::from(*handle),
                    info.state(),
                    info.modes().len()
                ));
                infos.push(info);
            }
            let candidates: Vec<ConnectorCandidate> = infos
                .iter()
                .map(|i| ConnectorCandidate {
                    name: i.to_string(),
                    connected: i.state() == connector::State::Connected,
                })
                .collect();
            if let Some(name) = request.connector.as_deref() {
                connector_seen |= candidates.iter().any(|c| c.name.eq_ignore_ascii_case(name));
            }
            let chosen = pick_connector(&candidates, request.connector.as_deref())
                .map_err(|e| e.context(&path))?;
            let Some(index) = chosen else {
                notes.push(format!("{path}: no matching connected connector"));
                continue;
            };
            let info = &infos[index];
            let name = candidates[index].name.clone();
            let modes = info.modes();
            for m in modes {
                let (w, h) = m.size();
                trace(&format!(
                    "    mode {w}x{h}@{} clock={} htotal={} vtotal={}{}",
                    m.vrefresh(),
                    m.clock(),
                    m.hsync().2,
                    m.vsync().2,
                    if m.mode_type().contains(ModeTypeFlags::PREFERRED) {
                        " (preferred)"
                    } else {
                        ""
                    }
                ));
            }
            let mode_candidates: Vec<ModeCandidate> = modes
                .iter()
                .map(|m| {
                    let (w, h) = m.size();
                    ModeCandidate {
                        width: u32::from(w),
                        height: u32::from(h),
                        refresh_hz: m.vrefresh(),
                        preferred: m.mode_type().contains(ModeTypeFlags::PREFERRED),
                    }
                })
                .collect();
            let mode_index = pick_mode(&mode_candidates, request.mode, request.prefer_mode)
                .map_err(|e| e.context(format!("{name} on {path}")))?;
            let mode = mode_candidates[mode_index];
            let timing = &modes[mode_index];
            let refresh_period = refresh_period(timing.clock(), timing.hsync().2, timing.vsync().2);
            return Ok(KmsTarget {
                refresh_period,
                connector_id: u32::from(info.handle()),
                connector_name: name,
                width: mode.width,
                height: mode.height,
                vrefresh_hz: mode.refresh_hz,
                card,
                path,
                plane: request.plane,
            });
        }
        Err(no_target_error(
            &notes,
            saw_kms_card,
            request.connector.as_deref(),
            connector_seen,
        ))
    }

    impl KmsTarget {
        /// Explicit master: a no-op if opening the card already made this
        /// process master, and a clear failure (EACCES/EBUSY) if something
        /// else (sway under greetd, usually) still owns the display.
        pub fn assert_master(&self) -> Result<(), KmsError> {
            self.card.acquire_master_lock().map_err(|e| {
                KmsError::failed(format!(
                    "not DRM master on {} ({e}); is the greetd/sway kiosk session still \
                     running? stop it first: sudo systemctl stop greetd.service",
                    self.path
                ))
            })
        }

        /// One-line summary for logs.
        pub fn describe(&self) -> String {
            format!(
                "{} {} connector_id={} mode={}x{}@{}",
                self.path,
                self.connector_name,
                self.connector_id,
                self.width,
                self.height,
                self.vrefresh_hz
            )
        }
    }

    /// The kernel's timestamp of one vblank on the display's CRTC.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Vblank {
        pub sequence: u32,
        /// CLOCK_MONOTONIC, as the kernel reports it.
        pub monotonic: std::time::Duration,
    }

    /// A second handle on the target's card (a dup of its fd: same open
    /// file, so same DRM master) for what the viewer needs at runtime: vblank
    /// timing and connector state. Safe to drop at any time; only the
    /// original fd in [`KmsTarget`] is shared with Mesa.
    pub struct KmsDisplay {
        card: Card,
        connector_id: u32,
        connector_name: String,
        width: u32,
        height: u32,
        vrefresh_hz: u32,
        refresh_period: std::time::Duration,
        /// Index of the CRTC driving the connector (the "pipe" the vblank
        /// ioctl addresses); resolved lazily because Mesa picks the CRTC at
        /// its first modeset.
        pipe: Option<u32>,
        /// The CRTC itself, cached at the first power change (it is resolved
        /// while lit and stays the connector's CRTC while inactive).
        crtc_handle: Option<drm::control::crtc::Handle>,
    }

    impl KmsTarget {
        pub fn display_handle(&self) -> Result<KmsDisplay, KmsError> {
            let file = self
                .card
                .0
                .try_clone()
                .map_err(|e| KmsError::failed(format!("dup {}: {e}", self.path)))?;
            Ok(KmsDisplay {
                card: Card(file),
                connector_id: self.connector_id,
                connector_name: self.connector_name.clone(),
                width: self.width,
                height: self.height,
                vrefresh_hz: self.vrefresh_hz,
                refresh_period: self.refresh_period,
                pipe: None,
                crtc_handle: None,
            })
        }
    }

    impl KmsDisplay {
        fn connector(&self, force_probe: bool) -> Result<connector::Info, KmsError> {
            self.card
                .get_connector(
                    connector::Handle::from(
                        drm::control::RawResourceHandle::new(self.connector_id)
                            .ok_or_else(|| KmsError::failed("connector id 0"))?,
                    ),
                    force_probe,
                )
                .map_err(|e| {
                    KmsError::failed(format!("query connector {}: {e}", self.connector_id))
                })
        }

        fn crtc(&self) -> Result<drm::control::crtc::Handle, KmsError> {
            let info = self.connector(false)?;
            let encoder = info
                .current_encoder()
                .ok_or_else(|| KmsError::failed("connector has no active encoder yet"))?;
            self.card
                .get_encoder(encoder)
                .map_err(|e| KmsError::failed(format!("query encoder: {e}")))?
                .crtc()
                .ok_or_else(|| KmsError::failed("encoder drives no CRTC yet"))
        }

        fn resolve_pipe(&self) -> Result<u32, KmsError> {
            let crtc = self.crtc()?;
            let res = self
                .card
                .resource_handles()
                .map_err(|e| KmsError::failed(format!("query resources: {e}")))?;
            res.crtcs()
                .iter()
                .position(|c| *c == crtc)
                .map(|i| i as u32)
                .ok_or_else(|| KmsError::failed("CRTC not in the card's list"))
        }

        /// Exact vblank period of the mode in use.
        pub fn refresh_period(&self) -> std::time::Duration {
            self.refresh_period
        }

        /// Block until the next vblank of the CRTC showing this connector and
        /// return the kernel's timestamp for it. The kernel bounds the wait
        /// (about 3 s, then EBUSY), so a stopped CRTC is an error, not a hang.
        pub fn wait_next_vblank(&mut self) -> Result<Vblank, KmsError> {
            self.vblank_query(1)
        }

        /// The most recent vblank (its sequence number and timestamp),
        /// without waiting: `DRM_IOCTL_WAIT_VBLANK` relative 0 returns at once.
        pub fn last_vblank(&mut self) -> Result<Vblank, KmsError> {
            self.vblank_query(0)
        }

        fn vblank_query(&mut self, relative: u32) -> Result<Vblank, KmsError> {
            use drm::{VblankWaitFlags, VblankWaitTarget};
            let pipe = match self.pipe {
                Some(pipe) => pipe,
                None => {
                    let pipe = self.resolve_pipe()?;
                    self.pipe = Some(pipe);
                    pipe
                }
            };
            let reply = self
                .card
                .wait_vblank(
                    VblankWaitTarget::Relative(relative),
                    VblankWaitFlags::empty(),
                    pipe,
                    0,
                )
                .map_err(|e| {
                    // Re-resolve next time in case the CRTC changed.
                    self.pipe = None;
                    KmsError::failed(format!("wait for vblank on pipe {pipe}: {e}"))
                })?;
            let monotonic = reply
                .time()
                .ok_or_else(|| KmsError::failed("vblank reply carried no timestamp"))?;
            Ok(Vblank {
                sequence: reply.frame(),
                monotonic,
            })
        }

        /// The connector this display drives, e.g. `HDMI-A-2`.
        pub fn connector_name(&self) -> &str {
            &self.connector_name
        }

        /// The mode in use, `WxH@Hz`.
        pub fn mode_label(&self) -> String {
            format!("{}x{}@{}", self.width, self.height, self.vrefresh_hz)
        }

        /// Switch the panel off (`on = false`) or back on with a full
        /// modeset (`on = true`), as one blocking atomic commit on the CRTC
        /// driving the connector.
        ///
        /// Atomic `ACTIVE` is the kernel's power switch on atomic drivers
        /// (vc4 is one); the legacy DPMS property only maps onto it, and the
        /// kernel documents its read-back as unreliable. Off commits
        /// `ACTIVE = 0`. On commits what Mesa's display WSI commits when it
        /// first takes the display: a fresh `MODE_ID` blob of the mode in
        /// use, the connector's `CRTC_ID` and `ACTIVE = 1`, with
        /// `ALLOW_MODESET`. That is the startup modeset, the one that drives
        /// the HDMI link up from nothing. The plane keeps Mesa's last
        /// framebuffer; Mesa's next present is an ordinary flip on a running
        /// CRTC. Mesa never learns the CRTC was off: it would fail that
        /// flip and lose the swapchain (25.3+), or fall back to its own
        /// modeset (25.2-) and light the panel behind the viewer's back, so
        /// the caller must present nothing while the CRTC is off.
        pub fn set_active(&mut self, on: bool) -> Result<(), KmsError> {
            use drm::control::AtomicCommitFlags;
            use drm::control::atomic::AtomicModeReq;
            use drm::control::property::Value;
            // Mesa's WSI already made this open file an atomic client; say
            // so ourselves too, so this works against any Mesa.
            self.card
                .set_client_capability(drm::ClientCapability::Atomic, true)
                .map_err(|e| KmsError::failed(format!("enable atomic modesetting: {e}")))?;
            let crtc = self.cached_crtc()?;
            let connector = self.connector_handle()?;
            let mut req = AtomicModeReq::new();
            let mut blob = None;
            if on {
                let mode = self.mode_in_use(crtc)?;
                let Value::Blob(id) = self
                    .card
                    .create_property_blob(&mode)
                    .map_err(|e| KmsError::failed(format!("create mode blob: {e}")))?
                else {
                    return Err(KmsError::failed("mode blob is not a blob"));
                };
                blob = Some(id);
                req.add_property(crtc, self.property(crtc, "MODE_ID")?.0, Value::Blob(id));
                req.add_property(
                    connector,
                    self.property(connector, "CRTC_ID")?.0,
                    Value::CRTC(Some(crtc)),
                );
            }
            req.add_property(crtc, self.property(crtc, "ACTIVE")?.0, Value::Boolean(on));
            let result = self
                .card
                .atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)
                .map_err(|e| {
                    KmsError::failed(format!(
                        "atomic commit ACTIVE={} on CRTC {} ({}): {e}",
                        u8::from(on),
                        u32::from(crtc),
                        self.connector_name
                    ))
                });
            if let Some(id) = blob {
                // The committed state holds its own reference.
                let _ = self.card.destroy_property_blob(id);
            }
            result
        }

        /// Whether a CRTC drives the connector (resolved and cached). Before
        /// Mesa's first present this is whatever the console left: none when
        /// the console never lit this connector.
        pub fn crtc_routed(&mut self) -> bool {
            self.cached_crtc().is_ok()
        }

        /// The CRTC's atomic `ACTIVE` property as the kernel holds it now
        /// (a read-back that, unlike legacy DPMS, reflects the state).
        pub fn crtc_active(&mut self) -> Result<bool, KmsError> {
            let crtc = self.cached_crtc()?;
            Ok(self.property(crtc, "ACTIVE")?.1 != 0)
        }

        fn cached_crtc(&mut self) -> Result<drm::control::crtc::Handle, KmsError> {
            if let Some(crtc) = self.crtc_handle {
                return Ok(crtc);
            }
            let crtc = self.crtc()?;
            self.crtc_handle = Some(crtc);
            Ok(crtc)
        }

        fn connector_handle(&self) -> Result<connector::Handle, KmsError> {
            Ok(connector::Handle::from(
                drm::control::RawResourceHandle::new(self.connector_id)
                    .ok_or_else(|| KmsError::failed("connector id 0"))?,
            ))
        }

        /// The mode the CRTC holds (it keeps it while inactive), else the
        /// connector's entry for the mode this process chose.
        fn mode_in_use(
            &self,
            crtc: drm::control::crtc::Handle,
        ) -> Result<drm::control::Mode, KmsError> {
            let matches = |m: &drm::control::Mode| {
                let (w, h) = m.size();
                u32::from(w) == self.width
                    && u32::from(h) == self.height
                    && m.vrefresh() == self.vrefresh_hz
            };
            if let Ok(info) = self.card.get_crtc(crtc)
                && let Some(mode) = info.mode()
                && matches(&mode)
            {
                return Ok(mode);
            }
            self.connector(false)?
                .modes()
                .iter()
                .copied()
                .find(matches)
                .ok_or_else(|| {
                    KmsError::failed(format!(
                        "{} no longer offers {}",
                        self.connector_name,
                        self.mode_label()
                    ))
                })
        }

        /// A property's handle and current value on an object, by name.
        fn property<H: drm::control::ResourceHandle>(
            &self,
            handle: H,
            name: &str,
        ) -> Result<(drm::control::property::Handle, u64), KmsError> {
            let props = self
                .card
                .get_properties(handle)
                .map_err(|e| KmsError::failed(format!("query properties: {e}")))?;
            let (ids, values) = props.as_props_and_values();
            for (id, value) in ids.iter().zip(values) {
                if let Ok(info) = self.card.get_property(*id)
                    && info.name().to_bytes() == name.as_bytes()
                {
                    return Ok((*id, *value));
                }
            }
            Err(KmsError::failed(format!("no {name} property")))
        }

        /// The connector reads connected (kernel's cached state; no probe).
        pub fn is_connected(&self) -> bool {
            self.connector(false)
                .is_ok_and(|info| info.state() == connector::State::Connected)
        }

        /// The display this process was started for is still there: the
        /// connector is connected and still lists the mode in use.
        ///
        /// Without `force_probe` this reads the kernel's last known state,
        /// which hot-plug detection keeps current for the connection status
        /// but not for the mode list: that is refreshed only by a probe, so a
        /// panel swapped between two checks would look unchanged. A forced
        /// probe re-reads the EDID over DDC (tens of ms; needs DRM master,
        /// which this dup'd fd shares), so the caller forces one only once
        /// per photo, at the dwell deadline.
        pub fn check_unchanged(&self, force_probe: bool) -> Result<(), KmsError> {
            let info = self.connector(force_probe)?;
            if info.state() != connector::State::Connected {
                return Err(KmsError::no_connected_display(format!(
                    "connector {info} is {:?}",
                    info.state()
                )));
            }
            let still_offered = info.modes().iter().any(|m| {
                let (w, h) = m.size();
                u32::from(w) == self.width
                    && u32::from(h) == self.height
                    && m.vrefresh() == self.vrefresh_hz
            });
            if !still_offered {
                return Err(KmsError::failed(format!(
                    "connector {info} no longer offers {}x{}@{}",
                    self.width, self.height, self.vrefresh_hz
                )));
            }
            Ok(())
        }
    }

    /// Build the `SurfaceTargetUnsafe::Drm` for `target`: checks the Vulkan
    /// extensions, resolves the connector through `vkGetDrmDisplayEXT`, and
    /// picks the exact refresh value (mHz) and display plane that wgpu-hal
    /// will compare against.
    ///
    /// # Safety
    /// The returned target carries `target`'s raw fd. The caller must keep
    /// `target` alive until after `instance` (and everything created from
    /// it) has been dropped; see module rule 2.
    pub unsafe fn surface_target(
        instance: &wgpu::Instance,
        target: &KmsTarget,
        trace: &mut dyn FnMut(&str),
    ) -> Result<wgpu::SurfaceTargetUnsafe, KmsError> {
        use wgpu::hal::api::Vulkan;
        let fd = target.card.as_fd().as_raw_fd();
        let hal = unsafe { instance.as_hal::<Vulkan>() }.ok_or_else(|| {
            KmsError::failed("the wgpu instance is not Vulkan; DRM presentation needs WGPU_BACKEND=vulkan (or unset)")
        })?;
        let shared = hal.shared_instance();
        let exts = shared.extensions();
        for required in [ash::khr::display::NAME, ash::ext::acquire_drm_display::NAME] {
            if !exts.contains(&required) {
                return Err(KmsError::failed(format!(
                    "Vulkan instance lacks {} (driver does not advertise it)",
                    required.to_string_lossy()
                )));
            }
        }
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .map_err(|e| KmsError::failed(format!("no Vulkan adapter: {e}")))?;
        let pd = {
            let a = unsafe { adapter.as_hal::<Vulkan>() }
                .ok_or_else(|| KmsError::failed("adapter is not Vulkan"))?;
            a.raw_physical_device()
        };
        let entry = shared.entry();
        let raw = shared.raw_instance();
        let acquire = ash::ext::acquire_drm_display::Instance::new(entry, raw);
        let display_ext = ash::khr::display::Instance::new(entry, raw);

        let display =
            unsafe { acquire.get_drm_display(pd, fd, target.connector_id) }.map_err(|e| {
                KmsError::failed(format!(
                    "vkGetDrmDisplayEXT(connector {}) failed: {e}",
                    target.connector_id
                ))
            })?;

        let modes = unsafe { display_ext.get_display_mode_properties(pd, display) }
            .map_err(|e| KmsError::failed(format!("vkGetDisplayModePropertiesKHR failed: {e}")))?;
        let vk_modes: Vec<(u32, u32, u32)> = modes
            .iter()
            .map(|m| {
                let p = m.parameters;
                trace(&format!(
                    "  vulkan mode {}x{} refresh={} mHz",
                    p.visible_region.width, p.visible_region.height, p.refresh_rate
                ));
                (
                    p.visible_region.width,
                    p.visible_region.height,
                    p.refresh_rate,
                )
            })
            .collect();
        let refresh =
            closest_refresh_mhz(&vk_modes, target.width, target.height, target.vrefresh_hz)
                .ok_or_else(|| {
                    KmsError::failed(format!(
                        "Vulkan lists no {}x{} mode for {}",
                        target.width, target.height, target.connector_name
                    ))
                })?;

        let plane = match target.plane {
            Some(p) => p,
            None => {
                let planes =
                    unsafe { display_ext.get_physical_device_display_plane_properties(pd) }
                        .map_err(|e| {
                            KmsError::failed(format!(
                                "vkGetPhysicalDeviceDisplayPlanePropertiesKHR failed: {e}"
                            ))
                        })?;
                let candidates: Vec<PlaneCandidate> = planes
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let supported = unsafe {
                            display_ext.get_display_plane_supported_displays(pd, i as u32)
                        }
                        .unwrap_or_default();
                        let candidate = PlaneCandidate {
                            current: p.current_display == display,
                            supports: supported.contains(&display),
                        };
                        trace(&format!(
                            "  vulkan plane {i}: current_display={} supports_target={}",
                            if candidate.current {
                                "target"
                            } else {
                                "other/none"
                            },
                            candidate.supports
                        ));
                        candidate
                    })
                    .collect();
                pick_plane(&candidates).ok_or_else(|| {
                    KmsError::failed("no Vulkan display plane supports the target display")
                })?
            }
        };
        trace(&format!(
            "creating DRM surface: plane={plane} refresh={refresh} mHz"
        ));
        Ok(wgpu::SurfaceTargetUnsafe::Drm {
            fd,
            plane,
            connector_id: target.connector_id,
            width: target.width,
            height: target.height,
            refresh_rate: refresh,
        })
    }
}

#[cfg(target_os = "linux")]
pub use device::{Card, KmsDisplay, KmsRequest, KmsTarget, Vblank, open_target, surface_target};

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str, connected: bool) -> ConnectorCandidate {
        ConnectorCandidate {
            name: name.into(),
            connected,
        }
    }

    fn mode(w: u32, h: u32, hz: u32, preferred: bool) -> ModeCandidate {
        ModeCandidate {
            width: w,
            height: h,
            refresh_hz: hz,
            preferred,
        }
    }

    #[test]
    fn mode_request_parses_size_and_optional_refresh() {
        assert_eq!(
            "3840x2160@60".parse::<ModeRequest>().unwrap(),
            ModeRequest {
                width: 3840,
                height: 2160,
                refresh_hz: Some(60)
            }
        );
        assert_eq!(
            "1920X1080".parse::<ModeRequest>().unwrap(),
            ModeRequest {
                width: 1920,
                height: 1080,
                refresh_hz: None
            }
        );
        for bad in [
            "",
            "3840",
            "3840x",
            "x2160",
            "3840x2160@",
            "0x2160",
            "axb",
            "3840x2160@sixty",
        ] {
            assert!(
                bad.parse::<ModeRequest>().is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert_eq!(
            "3840x2160@60".parse::<ModeRequest>().unwrap().to_string(),
            "3840x2160@60"
        );
    }

    #[test]
    fn connector_defaults_to_first_connected() {
        let c = [conn("HDMI-A-1", false), conn("HDMI-A-2", true)];
        assert_eq!(pick_connector(&c, None).unwrap(), Some(1));
    }

    #[test]
    fn connector_none_connected_falls_through_to_next_card() {
        let c = [conn("HDMI-A-1", false), conn("HDMI-A-2", false)];
        assert_eq!(pick_connector(&c, None).unwrap(), None);
        assert_eq!(pick_connector(&[], None).unwrap(), None);
    }

    #[test]
    fn named_connector_matches_case_insensitively() {
        let c = [conn("HDMI-A-1", true), conn("HDMI-A-2", true)];
        assert_eq!(pick_connector(&c, Some("hdmi-a-2")).unwrap(), Some(1));
        assert_eq!(pick_connector(&c, Some("DSI-1")).unwrap(), None);
    }

    #[test]
    fn named_but_disconnected_connector_is_a_hard_error() {
        let c = [conn("HDMI-A-1", false), conn("HDMI-A-2", true)];
        assert!(pick_connector(&c, Some("HDMI-A-1")).is_err());
    }

    #[test]
    fn mode_prefers_preferred_then_first() {
        let m = [mode(1920, 1080, 60, false), mode(3840, 2160, 60, true)];
        assert_eq!(pick_mode(&m, None, None).unwrap(), 1);
        let m = [mode(1920, 1080, 60, false), mode(3840, 2160, 30, false)];
        assert_eq!(pick_mode(&m, None, None).unwrap(), 0);
        assert!(pick_mode(&[], None, None).is_err());
    }

    #[test]
    fn explicit_mode_must_match_exactly() {
        let m = [
            mode(3840, 2160, 30, true),
            mode(3840, 2160, 60, false),
            mode(1920, 1080, 60, false),
        ];
        let req = |s: &str| Some(s.parse::<ModeRequest>().unwrap());
        assert_eq!(pick_mode(&m, req("3840x2160@60"), None).unwrap(), 1);
        // No refresh in the request: the first mode of that size.
        assert_eq!(pick_mode(&m, req("3840x2160"), None).unwrap(), 0);
        assert!(pick_mode(&m, req("2560x1440"), None).is_err());
        assert!(pick_mode(&m, req("1920x1080@50"), None).is_err());
    }

    #[test]
    fn preference_beats_edid_preferred_when_listed() {
        let kiosk = Some("3840x2160@60".parse::<ModeRequest>().unwrap());
        // A TV whose first DTD (EDID-preferred) is 1080p60 but that offers 4K60.
        let m = [
            mode(1920, 1080, 60, true),
            mode(3840, 2160, 30, false),
            mode(3840, 2160, 60, false),
        ];
        assert_eq!(pick_mode(&m, None, kiosk).unwrap(), 2);
        // Not offered: fall back to the EDID-preferred mode, not an error.
        let m = [mode(1920, 1080, 60, true), mode(3840, 2160, 30, false)];
        assert_eq!(pick_mode(&m, None, kiosk).unwrap(), 0);
        // An explicit request still wins and still must exist.
        let req = Some("3840x2160@30".parse::<ModeRequest>().unwrap());
        assert_eq!(pick_mode(&m, req, kiosk).unwrap(), 1);
        let req = Some("2560x1440".parse::<ModeRequest>().unwrap());
        assert!(pick_mode(&m, req, kiosk).is_err());
    }

    #[test]
    fn among_equal_matches_the_edid_preferred_one_wins() {
        let kiosk = Some("3840x2160@60".parse::<ModeRequest>().unwrap());
        // 60 and 59.94 Hz both read 60 from KMS.
        let m = [
            mode(3840, 2160, 60, false),
            mode(3840, 2160, 60, true),
            mode(1920, 1080, 60, false),
        ];
        assert_eq!(pick_mode(&m, None, kiosk).unwrap(), 1);
    }

    #[test]
    fn only_a_missing_display_is_worth_waiting_for() {
        let c = [conn("HDMI-A-1", false)];
        let err = pick_connector(&c, Some("HDMI-A-1")).unwrap_err();
        assert_eq!(err.kind(), KmsErrorKind::NoConnectedDisplay);
        let err = pick_mode(&[], None, None).unwrap_err();
        assert_eq!(err.kind(), KmsErrorKind::Failed);
    }

    #[test]
    fn refresh_period_comes_from_the_mode_timings() {
        // CTA 3840x2160@60: 594 MHz, 4400 x 2250.
        assert_eq!(
            refresh_period(594_000, 4400, 2250),
            std::time::Duration::from_nanos(16_666_666)
        );
        // The 59.94 Hz variant of the same timings (594/1.001 MHz).
        let ntsc = refresh_period(593_407, 4400, 2250);
        assert!(ntsc > std::time::Duration::from_nanos(16_683_000));
        assert!(ntsc < std::time::Duration::from_nanos(16_684_000));
    }

    #[test]
    fn a_misspelled_connector_fails_instead_of_waiting() {
        let notes = vec!["/dev/dri/card1: no matching connected connector".to_string()];
        let err = no_target_error(&notes, true, Some("HDMI-1"), false);
        assert_eq!(err.kind(), KmsErrorKind::Failed);
        assert!(err.to_string().contains("HDMI-1"), "{err}");
        // An existing but unplugged named connector is still waited for.
        let err = no_target_error(&notes, true, Some("HDMI-A-1"), true);
        assert_eq!(err.kind(), KmsErrorKind::NoConnectedDisplay);
        // No name: wait if a card has connectors at all.
        assert_eq!(
            no_target_error(&notes, true, None, false).kind(),
            KmsErrorKind::NoConnectedDisplay
        );
        assert_eq!(
            no_target_error(&notes, false, None, false).kind(),
            KmsErrorKind::Failed
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_display_polls_until_the_panel_appears() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let poll = std::time::Duration::from_secs(1);
        let start = tokio::time::Instant::now();
        let mut attempts = 0;
        let mut waits = 0;
        let got = wait_for_display(
            || {
                attempts += 1;
                if attempts < 4 {
                    Err(KmsError::no_connected_display("HDMI-A-1 is Disconnected"))
                } else {
                    Ok("target")
                }
            },
            poll,
            &cancel,
            |_| waits += 1,
        )
        .await
        .unwrap();
        assert_eq!(got, Some("target"));
        assert_eq!((attempts, waits), (4, 3));
        assert_eq!(start.elapsed(), poll * 3);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_display_returns_other_errors_at_once() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let err = wait_for_display(
            || -> Result<(), KmsError> { Err(KmsError::failed("not DRM master")) },
            std::time::Duration::from_secs(1),
            &cancel,
            |_| panic!("must not wait"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), KmsErrorKind::Failed);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_display_stops_on_cancel() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        let got = wait_for_display(
            || -> Result<(), KmsError> { Err(KmsError::no_connected_display("no panel")) },
            std::time::Duration::from_secs(1),
            &cancel,
            move |_| token.cancel(),
        )
        .await
        .unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn refresh_picks_the_nearest_vulkan_value_at_the_exact_size() {
        let vk = [
            (3840, 2160, 30_000),
            (3840, 2160, 59_940),
            (3840, 2160, 60_000),
            (1920, 1080, 60_000),
        ];
        assert_eq!(closest_refresh_mhz(&vk, 3840, 2160, 60), Some(60_000));
        assert_eq!(closest_refresh_mhz(&vk, 3840, 2160, 30), Some(30_000));
        let ntsc_only = [(3840, 2160, 59_940)];
        assert_eq!(
            closest_refresh_mhz(&ntsc_only, 3840, 2160, 60),
            Some(59_940)
        );
        assert_eq!(closest_refresh_mhz(&vk, 2560, 1440, 60), None);
    }

    #[test]
    fn plane_is_first_that_shows_or_supports_the_display() {
        let p = |current, supports| PlaneCandidate { current, supports };
        assert_eq!(
            pick_plane(&[p(false, false), p(false, true), p(true, true)]),
            Some(1)
        );
        assert_eq!(pick_plane(&[p(true, false)]), Some(0));
        assert_eq!(pick_plane(&[p(false, false)]), None);
        assert_eq!(pick_plane(&[]), None);
    }
}
