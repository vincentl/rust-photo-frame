use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct FileIndex {
    set: HashSet<PathBuf>,
    list: Vec<PathBuf>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if it was newly inserted.
    pub fn add(&mut self, p: PathBuf) -> bool {
        if self.set.insert(p.clone()) {
            self.list.push(p);
            true
        } else {
            false
        }
    }

    /// Returns true if it was present and removed.
    pub fn remove(&mut self, p: &Path) -> bool {
        if self.set.remove(p) {
            if let Some(i) = self.list.iter().position(|x| x == p) {
                self.list.swap_remove(i);
            }
            true
        } else {
            false
        }
    }

    /// Snapshot (e.g., feed a viewer)
    pub fn all(&self) -> Vec<PathBuf> {
        self.list.clone()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}
