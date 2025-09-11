use anyhow::{Context, Result};
use std::{fs, io::BufReader, path::{Path, PathBuf}};

#[derive(Debug, Clone)]
pub struct ImageMeta {
    pub path: PathBuf,
    pub raw_w: u32,
    pub raw_h: u32,
    pub display_w: u32,
    pub display_h: u32,
    pub exif_orientation: u16,
    pub exif_label: &'static str,
}

/// FAST: read only header for dimensions; optionally read EXIF.
pub fn get_image_meta_fast(path: &Path, read_exif: bool) -> Result<ImageMeta> {
    let (raw_w, raw_h) = image::image_dimensions(path)?;
    let o = if read_exif { read_exif_orientation(path).unwrap_or(1) } else { 1 };
    let swap = matches!(o, 5 | 6 | 7 | 8);
    let (display_w, display_h) = if swap { (raw_h, raw_w) } else { (raw_w, raw_h) };
    Ok(ImageMeta {
        path: path.to_path_buf(),
        raw_w, raw_h, display_w, display_h,
        exif_orientation: o,
        exif_label: exif_orientation_label(o),
    })
}


fn read_exif_orientation(path: &Path) -> Option<u16> {
    let f = fs::File::open(path).ok()?;
    let mut buf = BufReader::new(f);
    let reader = exif::Reader::new().read_from_container(&mut buf).ok()?;
    use exif::{In, Tag, Value};
    let field = reader.get_field(Tag::Orientation, In::PRIMARY)?;
    match &field.value {
        Value::Short(arr) if !arr.is_empty() => Some(arr[0]),
        Value::Long(arr)  if !arr.is_empty() => Some(arr[0] as u16),
        _ => Some(1),
    }
}

#[inline]
fn exif_orientation_label(v: u16) -> &'static str {
    match v {
        1 => "Normal (1)",
        2 => "Mirrored horizontal (2)",
        3 => "Rotated 180° (3)",
        4 => "Mirrored vertical (4)",
        5 => "Mirrored horizontal + Rotated 270° (5)",
        6 => "Rotated 90° CW (6)",
        7 => "Mirrored horizontal + Rotated 90° (7)",
        8 => "Rotated 270° CW (8)",
        _ => "Unknown",
    }
}
