use std::path::PathBuf;

#[derive(Debug)]
pub enum InventoryEvent {
    PhotoAdded(PathBuf),
    PhotoRemoved(PathBuf),
}

#[derive(Debug)]
pub struct LoadPhoto(pub PathBuf);

#[derive(Debug)]
pub struct PreparedPhoto {
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct PhotoLoaded(pub PreparedPhoto);

#[derive(Debug)]
pub struct InvalidPhoto(pub PathBuf);

/// Emitted by the viewer after a photo is shown (for now, immediately).
#[derive(Debug)]
pub struct Displayed(pub PathBuf);
