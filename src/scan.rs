use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const SUPPORTED_EXTS: &[&str] = &["jpg","jpeg","png","gif","webp","bmp","tiff"];

pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| SUPPORTED_EXTS.contains(&s.to_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn scan_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in dirs {
        for entry in WalkDir::new(d).follow_links(false).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_file() {
                let p = entry.into_path();
                if is_supported_image(&p) {
                    files.push(p);
                }
            }
        }
    }
    files
}
