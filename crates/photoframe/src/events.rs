use std::path::PathBuf;

use config_model::control::{PowerState, Reply, WifiOverlay};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ViewerState {
    Asleep,
    Awake,
}

#[derive(Debug)]
#[allow(clippy::enum_variant_names)] // `Photo*` are the inventory's event names
pub enum InventoryEvent {
    PhotoAdded(PhotoInfo),
    PhotoRemoved(PathBuf),
    /// A photo's displayed size learned after its `PhotoAdded` (portrait
    /// pairing). Applies only to known paths, so it can never resurrect a
    /// removed photo; see [`MetaSource`] for which value wins.
    PhotoMeta {
        path: PathBuf,
        display_size: (u32, u32),
        source: MetaSource,
    },
}

/// Who measured a `PhotoMeta` size. One authority (design R10): the decode
/// is the truth and always overwrites; the startup header pass only fills a
/// size that is still unknown, so a stale pass result loses to a fresh
/// `PhotoAdded` read or a decode.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MetaSource {
    /// The startup header pass (fills only while the size is unknown).
    Header,
    /// The loader, after a successful decode (D3's `source_size`; always
    /// overwrites, which also corrects an in-place rotation).
    Decoded,
}

/// The matting canvas `(W_c, H_c)` (screen x oversample, within the texture
/// limit) as published by the viewer, plus a version bumped on every change.
/// Portrait pairing reads it at each decision; until the first publish
/// nothing is pairable (there is deliberately no 16:9 default). Cloning
/// shares the slot.
#[derive(Clone, Debug, Default)]
pub struct CanvasShape {
    slot: Arc<Mutex<CanvasShapeState>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CanvasShapeState {
    size: Option<(u32, u32)>,
    version: u64,
}

impl CanvasShape {
    /// Record the canvas the matting worker renders into.
    pub fn publish(&self, canvas_w: u32, canvas_h: u32) {
        if canvas_w == 0 || canvas_h == 0 {
            return;
        }
        if let Ok(mut state) = self.slot.lock()
            && state.size != Some((canvas_w, canvas_h))
        {
            state.size = Some((canvas_w, canvas_h));
            state.version += 1;
        }
    }

    /// The published canvas, or `None` before the first publish.
    pub fn get(&self) -> Option<(u32, u32)> {
        self.slot.lock().ok()?.size
    }

    /// Bumped on every change of the published canvas (0 = never published).
    pub fn version(&self) -> u64 {
        self.slot.lock().map_or(0, |state| state.version)
    }
}

/// Which filesystem timestamp `created_at` came from. The frame intentionally
/// ages photos by when the file was *staged to the frame*, not by EXIF capture
/// date, so this records whether we got the true birth time or a fallback.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CreatedSource {
    /// Filesystem birth time (`st_birthtime`) — staging time. The intended source.
    Birthtime,
    /// Fallback: filesystem mtime (birth time unsupported/unavailable).
    Mtime,
    /// Fallback: wall-clock now (metadata unreadable) — photo treated as brand new.
    Now,
}

impl CreatedSource {
    /// Stable lowercase name for metric logging.
    pub fn as_str(self) -> &'static str {
        match self {
            CreatedSource::Birthtime => "birthtime",
            CreatedSource::Mtime => "mtime",
            CreatedSource::Now => "now",
        }
    }
}

impl std::fmt::Display for CreatedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct PhotoInfo {
    pub path: PathBuf,
    pub created_at: SystemTime,
    /// Where `created_at` came from (birth time vs. a fallback).
    pub created_source: CreatedSource,
    /// True when the photo was discovered by the watcher while the slideshow is
    /// running (eligible for the priority FIFO so it debuts promptly); false for
    /// the initial startup scan, which schedules straight onto the timeline.
    pub runtime_added: bool,
    /// Pixel size as displayed (EXIF orientation applied), from a header
    /// read; `None` when not known yet (or pairing is off, which reads no
    /// headers). A photo without one is not pairable.
    pub display_size: Option<(u32, u32)>,
}

/// A load request: one screen. `path` is the first appearance (drawn on the
/// left of a pair).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPhoto {
    pub path: PathBuf,
    pub priority: bool,
    /// Portrait pairing: the screen's second member; `None` for a solo screen.
    pub partner: Option<LoadPartner>,
}

/// The second member of a pair screen, with its own priority (design B2: a
/// screen is priority iff any member is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPartner {
    pub path: PathBuf,
    pub priority: bool,
}

#[derive(Debug, Clone)]
pub struct PreparedImageCpu {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Size of the original photo in its displayed orientation. Equal to
    /// `(width, height)` unless the loader decoded a JPEG at a reduced DCT
    /// scale; anything reasoning about the photo itself (fill eligibility,
    /// whether the pixels suffice for the canvas) must use this, not the
    /// decoded size.
    pub source_size: (u32, u32),
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PhotoLoaded {
    pub prepared: PreparedImageCpu,
    pub priority: bool,
    /// Portrait pairing: the decoded second member of a pair screen.
    pub partner: Option<LoadedPartner>,
}

impl PhotoLoaded {
    /// Decoded photos this screen carries: 1, or 2 for a pair.
    pub fn photos(&self) -> usize {
        1 + usize::from(self.partner.is_some())
    }
}

#[derive(Debug, Clone)]
pub struct LoadedPartner {
    pub prepared: PreparedImageCpu,
    pub priority: bool,
}

#[derive(Debug)]
pub struct InvalidPhoto(pub PathBuf);

/// Emitted by the viewer when a screen is shown: its first photo and, for a
/// pair screen, the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Displayed {
    pub path: PathBuf,
    pub partner: Option<PathBuf>,
}

/// A control-socket request the viewer itself has to act on (the control
/// task answers the rest; see `run_control_socket` in main.rs).
#[derive(Debug)]
pub enum ViewerCommand {
    SetState(ViewerState),
    ToggleState,
    /// DRM/KMS display power: switch it (`state: Some`) or report it
    /// (`None`). The viewer answers on `reply`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // read by the KMS loop
    DisplayPower {
        state: Option<PowerState>,
        output: Option<String>,
        reply: tokio::sync::oneshot::Sender<Reply>,
    },
    /// Show or refresh the Wi-Fi recovery overlay (content already validated).
    ShowWifiOverlay(Box<WifiOverlay>),
    HideWifiOverlay,
}
