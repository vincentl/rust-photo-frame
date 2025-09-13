//! Fast metadata-only scan of photo directories.
//! - Reads image dimensions via `image::image_dimensions` (header only, fast).
//! - Optionally reads EXIF orientation if present (best-effort).
//! - Does NOT decode full images.
//
// Returns a list of `PhotoMeta` so the viewer can size/plan without loading pixels.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use exif::{Reader as ExifReader, Tag};
use image::image_dimensions;
use walkdir::{DirEntry, WalkDir};

use crate::error::Error;

#[derive(Debug, Clone)]

/// Metadata about a discovered photo.
pub struct PhotoMeta {
    /// File path on disk.
    pub path: PathBuf,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// EXIF orientation (1 = normal).
    pub orientation: u32,
}

/// Returns `true` if `path` has an allowed image extension.
#[must_use]
fn is_supported_image(path: &Path) -> bool {
    let exts: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            exts.iter().any(|e| *e == ext)
        })
}

fn should_skip_dir(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    if !entry.file_type().is_dir() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|n| n.starts_with('.'))
}

/// Scan the given `paths` recursively (skips dot-directories).
/// # Errors
/// Returns [`Error::BadDir`] if a root is invalid, or [`Error::EmptyScan`] if nothing found.
pub fn scan(paths: &[PathBuf]) -> Result<Vec<PhotoMeta>, Error> {
    // Validate inputs
    let mut bad = Vec::new();
    for p in paths {
        if !p.exists() || !p.is_dir() {
            bad.push(p.clone());
        }
    }
    if !bad.is_empty() {
        let joined = bad
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::BadDir(joined));
    }

    let mut out = Vec::new();
    for root in paths {
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| !should_skip_dir(e))
        {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_supported_image(path) {
                continue;
            }

            // Dimensions via header only
            let Ok((w, h)) = image_dimensions(path) else {
                continue;
            };

            // Best-effort EXIF orientation
            let mut orientation: u32 = 1;
            if let Ok(file) = File::open(path) {
                let mut br = BufReader::new(file);
                if let Ok(exif) = ExifReader::new().read_from_container(&mut br)
                    && let Some(field) = exif.get_field(Tag::Orientation, exif::In::PRIMARY)
                    && let Some(v) = field.value.get_uint(0)
                {
                    orientation = v;
                }
            }

            out.push(PhotoMeta {
                path: path.to_path_buf(),
                width: w,
                height: h,
                orientation,
            });
        }
    }

    if out.is_empty() {
        return Err(Error::EmptyScan);
    }

    Ok(out)
}
