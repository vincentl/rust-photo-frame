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
        let bad = || KmsError(format!("invalid mode {s:?}; expected WxH[@Hz]"));
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
pub struct KmsError(pub String);

impl fmt::Display for KmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
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
        Some(i) if !candidates[i].connected => Err(KmsError(format!(
            "connector {} is not connected",
            candidates[i].name
        ))),
        other => Ok(other),
    }
}

/// One mode from a connector's KMS mode list.
#[derive(Clone, Copy, Debug)]
pub struct ModeCandidate {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub preferred: bool,
}

/// Choose a mode: an explicit request must match exactly (any refresh when
/// the request has none); otherwise the connector's preferred mode, else its
/// first mode.
pub fn pick_mode(modes: &[ModeCandidate], request: Option<ModeRequest>) -> Result<usize, KmsError> {
    if modes.is_empty() {
        return Err(KmsError("connector reports no modes".into()));
    }
    match request {
        Some(req) => modes
            .iter()
            .position(|m| {
                m.width == req.width
                    && m.height == req.height
                    && req.refresh_hz.is_none_or(|hz| m.refresh_hz == hz)
            })
            .ok_or_else(|| KmsError(format!("connector has no mode {req}"))),
        None => Ok(modes.iter().position(|m| m.preferred).unwrap_or(0)),
    }
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
        /// Vulkan display plane index.
        pub plane: Option<u32>,
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
            trace(&format!(
                "{path}: driver={driver} connectors={} crtcs={}",
                res.connectors().len(),
                res.crtcs().len()
            ));
            let mut infos = Vec::new();
            for handle in res.connectors() {
                let Ok(info) = card.get_connector(*handle, false) else {
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
            let chosen = pick_connector(&candidates, request.connector.as_deref())
                .map_err(|e| KmsError(format!("{e} on {path}")))?;
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
            let mode = pick_mode(&mode_candidates, request.mode)
                .map_err(|e| KmsError(format!("{name} on {path}: {e}")))?;
            let mode = mode_candidates[mode];
            return Ok(KmsTarget {
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
        Err(KmsError(format!(
            "no usable DRM card found:\n  {}",
            notes.join("\n  ")
        )))
    }

    impl KmsTarget {
        /// Explicit master: a no-op if opening the card already made this
        /// process master, and a clear failure (EACCES/EBUSY) if something
        /// else (sway under greetd, usually) still owns the display.
        pub fn assert_master(&self) -> Result<(), KmsError> {
            self.card.acquire_master_lock().map_err(|e| {
                KmsError(format!(
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
            KmsError("the wgpu instance is not Vulkan; DRM presentation needs WGPU_BACKEND=vulkan (or unset)".into())
        })?;
        let shared = hal.shared_instance();
        let exts = shared.extensions();
        for required in [ash::khr::display::NAME, ash::ext::acquire_drm_display::NAME] {
            if !exts.contains(&required) {
                return Err(KmsError(format!(
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
        .map_err(|e| KmsError(format!("no Vulkan adapter: {e}")))?;
        let pd = {
            let a = unsafe { adapter.as_hal::<Vulkan>() }
                .ok_or_else(|| KmsError("adapter is not Vulkan".into()))?;
            a.raw_physical_device()
        };
        let entry = shared.entry();
        let raw = shared.raw_instance();
        let acquire = ash::ext::acquire_drm_display::Instance::new(entry, raw);
        let display_ext = ash::khr::display::Instance::new(entry, raw);

        let display =
            unsafe { acquire.get_drm_display(pd, fd, target.connector_id) }.map_err(|e| {
                KmsError(format!(
                    "vkGetDrmDisplayEXT(connector {}) failed: {e}",
                    target.connector_id
                ))
            })?;

        let modes = unsafe { display_ext.get_display_mode_properties(pd, display) }
            .map_err(|e| KmsError(format!("vkGetDisplayModePropertiesKHR failed: {e}")))?;
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
                    KmsError(format!(
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
                            KmsError(format!(
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
                    KmsError("no Vulkan display plane supports the target display".into())
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
pub use device::{Card, KmsRequest, KmsTarget, open_target, surface_target};

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
        assert_eq!(pick_mode(&m, None).unwrap(), 1);
        let m = [mode(1920, 1080, 60, false), mode(3840, 2160, 30, false)];
        assert_eq!(pick_mode(&m, None).unwrap(), 0);
        assert!(pick_mode(&[], None).is_err());
    }

    #[test]
    fn explicit_mode_must_match_exactly() {
        let m = [
            mode(3840, 2160, 30, true),
            mode(3840, 2160, 60, false),
            mode(1920, 1080, 60, false),
        ];
        let req = |s: &str| Some(s.parse::<ModeRequest>().unwrap());
        assert_eq!(pick_mode(&m, req("3840x2160@60")).unwrap(), 1);
        // No refresh in the request: the first mode of that size.
        assert_eq!(pick_mode(&m, req("3840x2160")).unwrap(), 0);
        assert!(pick_mode(&m, req("2560x1440")).is_err());
        assert!(pick_mode(&m, req("1920x1080@50")).is_err());
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
