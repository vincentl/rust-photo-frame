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
