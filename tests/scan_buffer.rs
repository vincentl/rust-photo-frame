use std::fs;
use std::path::PathBuf;

use photoframe::{
    buffer::PhotoBuffer,
    scan::{ScanOptions, scan_with_options},
};
use tempfile::tempdir;

#[test]
fn scan_and_buffer_cooperate() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    fs::write(root.join("x.jpg"), b"x").unwrap();
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub").join("y.png"), b"x").unwrap();

    let paths = vec![root.to_path_buf()];
    let photos = scan_with_options(&paths, &ScanOptions::default()).unwrap();
    assert_eq!(photos.len(), 2);

    let mut buf = PhotoBuffer::from_vec(photos).unwrap();
    let a: PathBuf = buf.advance().clone();
    let b: PathBuf = buf.advance().clone();
    let c: PathBuf = buf.advance().clone();
    assert_eq!(a, root.join("x.jpg"));
    assert_eq!(b, root.join("sub").join("y.png"));
    assert_eq!(c, root.join("x.jpg"));
}
