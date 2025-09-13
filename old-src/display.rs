//! Display utilities: validation and glue to the render backend.
use std::path::PathBuf;

use tracing::warn;

/// Filter out missing or unreadable images.
///
/// Attempts to open and decode the image. Files that don't exist or
/// fail to decode are skipped with a warning.
#[must_use]
pub fn filter_valid_images(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        match image::open(p) {
            Ok(_) => out.push(p.clone()),
            Err(e) => {
                if p.exists() {
                    warn!(path=%p.display(), error=%e, "unreadable image; skipping");
                } else {
                    warn!(path=%p.display(), "missing image; skipping");
                }
            }
        }
    }
    out
}
