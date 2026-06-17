use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ViewerState {
    Asleep,
    Awake,
}

#[derive(Debug)]
pub enum InventoryEvent {
    PhotoAdded(PhotoInfo),
    PhotoRemoved(PathBuf),
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
}

#[derive(Debug, Clone)]
pub struct LoadPhoto {
    pub path: PathBuf,
    pub priority: bool,
}

#[derive(Debug, Clone)]
pub struct PreparedImageCpu {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PhotoLoaded {
    pub prepared: PreparedImageCpu,
    pub priority: bool,
}

#[derive(Debug)]
pub struct InvalidPhoto(pub PathBuf);

/// Emitted by the viewer after a photo is shown (for now, immediately).
#[derive(Debug)]
pub struct Displayed(pub PathBuf);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ViewerCommand {
    SetState(ViewerState),
    ToggleState,
}
