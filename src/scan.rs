//! Directory scanning utilities for discovering image files.

use std::path::{Path, PathBuf};

use walkdir::{DirEntry, WalkDir};

use crate::error::Error;

/// Options controlling directory scanning.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Whether to recurse into subdirectories.
    pub recursive: bool,
    /// Optional maximum recursion depth. `None` or `Some(0)` means unlimited.
    pub max_depth: Option<usize>,
    /// Optional override for allowed extensions (lowercase, without dot).
    pub exts: Option<Vec<&'static str>>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            recursive: true,
            max_depth: None,
            exts: None,
        }
    }
}

/// Return `true` if `path` has an allowed image extension.
#[must_use]
pub fn is_supported_image(path: &Path, exts: Option<&[&str]>) -> bool {
    let default_exts: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];
    let exts = exts.unwrap_or(default_exts);
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            exts.iter().any(|e| *e == ext)
        })
}

/// Scan the given `paths` for images using the provided options.
///
/// # Errors
/// Returns [`Error::BadDir`] if any path is missing or not a directory.
pub fn scan_with_options(paths: &[PathBuf], opts: &ScanOptions) -> Result<Vec<PathBuf>, Error> {
    // Validate inputs first (collect all bad ones).
    let mut bad = Vec::new();
    for p in paths {
        if !p.exists() || !p.is_dir() {
            bad.push(p.clone());
        }
    }
    if !bad.is_empty() {
        let joined = bad
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::BadDir(joined));
    }

    let mut out = Vec::new();
    for root in paths {
        // Depth handling
        let mut wd = WalkDir::new(root);
        if !opts.recursive {
            wd = wd.max_depth(1);
        } else if let Some(d) = opts.max_depth
            && d > 0
        {
            wd = wd.max_depth(d);
        }

        for entry in wd
            .into_iter()
            // Skip hidden dot-directories *below* the root only.
            .filter_entry(|e| !should_skip_dir(e))
            .flatten()
        {
            let path = entry.path();
            if path.is_file() && is_supported_image(path, opts.exts.as_deref()) {
                out.push(path.to_path_buf());
            }
        }
    }

    Ok(out)
}

fn should_skip_dir(entry: &DirEntry) -> bool {
    // Never skip the root; tempfile roots can be dot-dirs.
    if entry.depth() == 0 {
        return false;
    }
    // Skip typical hidden dot-directories like .git, .idea, etc.
    if !entry.file_type().is_dir() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .is_some_and(|n| n.starts_with('.'))
}
