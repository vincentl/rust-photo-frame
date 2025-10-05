use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug)]
pub enum InventoryEvent {
    PhotoAdded(PhotoInfo),
    PhotoRemoved(PathBuf),
}

#[derive(Debug, Clone)]
pub struct PhotoInfo {
    pub path: PathBuf,
    pub created_at: SystemTime,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepCommandSource {
    Manual,
    Schedule,
}

#[derive(Debug, Clone)]
pub enum ViewerCommand {
    ToggleSleep,
    GoToSleep(SleepCommandSource),
    AwakeNow(SleepCommandSource),
}
