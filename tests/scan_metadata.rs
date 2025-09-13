//! Integration tests for the fast metadata scan.
//! Creates a temporary directory with a tiny 1x1 PNG and verifies that
//! `scan()` returns one `PhotoMeta` with width/height 1 and default orientation 1.
//! Also verifies that non-image files are ignored, and that invalid roots error.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use photoframe::scan::{PhotoMeta, scan};

/// Return a path under a unique temporary directory for tests.
fn mk_tmp_dir() -> PathBuf {
    let mut dir = std::env::temp_dir();
    // Make a reasonably unique subdir name.
    let uniq = format!("photoframe_scan_test_{}", std::process::id());
    dir.push(uniq);
    // Best-effort cleanup: ignore errors if the dir already exists.
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Write a tiny 1x1 PNG to `path`.
fn write_1x1_png<P: AsRef<Path>>(path: P) {
    // A valid minimal 1x1 RGBA PNG.
    const PNG_BYTES: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0x78, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut f = fs::File::create(path).expect("create png file");
    f.write_all(PNG_BYTES).expect("write png");
}

#[test]
fn scan_finds_png_and_reports_dimensions() {
    let dir = mk_tmp_dir();
    let png = dir.join("tiny.png");
    let txt = dir.join("note.txt");

    write_1x1_png(&png);
    fs::write(&txt, "hello").expect("write txt");

    let metas = scan(std::slice::from_ref(&dir)).expect("scan should succeed");
    // Only the PNG should be picked up.
    assert_eq!(metas.len(), 1, "expected exactly one image in scan");
    let m: &PhotoMeta = &metas[0];
    assert!(
        m.path.ends_with("tiny.png"),
        "unexpected path: {:?}",
        m.path
    );
    assert_eq!(m.width, 1, "width should be 1");
    assert_eq!(m.height, 1, "height should be 1");
    assert_eq!(m.orientation, 1, "orientation default should be 1");
}

#[test]
fn scan_invalid_root_errors() {
    let bogus = PathBuf::from("/this/path/does/not/exist/for_photoframe_test");
    let err = scan(&[bogus]).expect_err("scan should error for invalid root");
    // We don't rely on a specific error variant; just check it stringifies.
    let s = err.to_string();
    assert!(!s.is_empty(), "error should have a message");
}
