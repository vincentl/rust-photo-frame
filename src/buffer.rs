//! Circular buffer over photo paths used to implement an infinite slideshow.

use std::path::PathBuf;

use crate::error::Error;

/// A simple circular buffer (ring) over photo paths.
#[derive(Debug, Clone)]
pub struct PhotoBuffer {
    items: Vec<PathBuf>,
    idx: usize,
}

impl PhotoBuffer {
    /// Construct a buffer from a list of paths.
    ///
    /// # Errors
    /// Returns [`Error::EmptyScan`] if `items` is empty.
    pub fn from_vec(items: Vec<PathBuf>) -> Result<Self, Error> {
        if items.is_empty() {
            return Err(Error::EmptyScan);
        }
        Ok(Self { items, idx: 0 })
    }

    /// Number of items contained.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Borrow the current item without advancing.
    #[must_use]
    pub fn peek(&self) -> Option<&PathBuf> {
        self.items.get(self.idx)
    }

    /// Advance and return the next item, wrapping at the end.
    pub fn advance(&mut self) -> &PathBuf {
        let out = &self.items[self.idx];
        self.idx = (self.idx + 1) % self.items.len();
        out
    }

    /// Reset iteration back to the start.
    pub const fn reset(&mut self) {
        self.idx = 0;
    }

    /// Borrow the internal list (read-only).
    #[must_use]
    pub fn as_slice(&self) -> &[PathBuf] {
        &self.items
    }
}
