use std::path::PathBuf;

#[derive(Debug)]
pub enum InventoryEvent {
    PhotoAdded(PathBuf),
    PhotoRemoved(PathBuf),
}

#[derive(Debug)]
pub struct LoadPhoto(pub PathBuf);

#[derive(Debug, Clone)]
pub struct PreparedImageCpu {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug)]
pub struct PhotoLoaded(pub PreparedImageCpu);

#[derive(Debug)]
pub struct InvalidPhoto(pub PathBuf);

/// Emitted by the viewer after a photo is shown (for now, immediately).
#[derive(Debug)]
pub struct Displayed(pub PathBuf);

