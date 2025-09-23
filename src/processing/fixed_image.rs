use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use image::{imageops, RgbaImage};

use crate::config::FixedImageFit;
use crate::processing::color::average_color_rgba;
use crate::processing::layout::{center_offset, resize_to_contain, resize_to_cover};

#[derive(Debug)]
pub struct FixedImageBackground {
    path: PathBuf,
    cache: Mutex<Option<CachedImage>>,
    average_color: OnceLock<image::Rgba<u8>>,
}

#[derive(Debug)]
struct CachedImage {
    width: u32,
    height: u32,
    max_dim: u32,
    fit: FixedImageFit,
    image: Arc<RgbaImage>,
}

impl CachedImage {
    fn matches(&self, width: u32, height: u32, max_dim: u32, fit: FixedImageFit) -> bool {
        self.width == width && self.height == height && self.max_dim == max_dim && self.fit == fit
    }
}

impl FixedImageBackground {
    pub fn new(path: PathBuf) -> Result<Self> {
        let metadata = fs::metadata(&path).with_context(|| {
            format!(
                "failed to read metadata for fixed background image at {}",
                path.display()
            )
        })?;
        anyhow::ensure!(
            metadata.is_file(),
            "fixed background image path {} must point to a file",
            path.display()
        );

        Ok(Self {
            path,
            cache: Mutex::new(None),
            average_color: OnceLock::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn canvas_for(
        &self,
        fit: FixedImageFit,
        canvas_w: u32,
        canvas_h: u32,
        max_dim: u32,
    ) -> Result<Arc<RgbaImage>> {
        if canvas_w == 0 || canvas_h == 0 {
            anyhow::bail!("canvas dimensions must be positive");
        }

        if let Some(hit) = self
            .cache
            .lock()
            .expect("fixed-image cache poisoned")
            .as_ref()
            .filter(|cached| cached.matches(canvas_w, canvas_h, max_dim, fit))
        {
            return Ok(Arc::clone(&hit.image));
        }

        let source = self.load_source()?;
        let avg = *self
            .average_color
            .get_or_init(|| average_color_rgba(&source));

        let prepared = match fit {
            FixedImageFit::Stretch => imageops::resize(
                &source,
                canvas_w,
                canvas_h,
                imageops::FilterType::CatmullRom,
            ),
            FixedImageFit::Cover => {
                let (bg_w, bg_h) =
                    resize_to_cover(canvas_w, canvas_h, source.width(), source.height(), max_dim);
                let resized =
                    imageops::resize(&source, bg_w, bg_h, imageops::FilterType::CatmullRom);
                if bg_w > canvas_w || bg_h > canvas_h {
                    let crop_x = (bg_w.saturating_sub(canvas_w)) / 2;
                    let crop_y = (bg_h.saturating_sub(canvas_h)) / 2;
                    imageops::crop_imm(&resized, crop_x, crop_y, canvas_w, canvas_h).to_image()
                } else if bg_w < canvas_w || bg_h < canvas_h {
                    let mut canvas = RgbaImage::from_pixel(canvas_w, canvas_h, avg);
                    let (ox, oy) = center_offset(bg_w, bg_h, canvas_w, canvas_h);
                    imageops::overlay(&mut canvas, &resized, ox as i64, oy as i64);
                    canvas
                } else {
                    resized
                }
            }
            FixedImageFit::Contain => {
                let (bg_w, bg_h) =
                    resize_to_contain(canvas_w, canvas_h, source.width(), source.height(), max_dim);
                let resized =
                    imageops::resize(&source, bg_w, bg_h, imageops::FilterType::CatmullRom);
                let mut canvas = RgbaImage::from_pixel(canvas_w, canvas_h, avg);
                let (ox, oy) = center_offset(bg_w, bg_h, canvas_w, canvas_h);
                imageops::overlay(&mut canvas, &resized, ox as i64, oy as i64);
                canvas
            }
        };

        let prepared = Arc::new(prepared);
        let mut cache = self.cache.lock().expect("fixed-image cache poisoned");
        *cache = Some(CachedImage {
            width: canvas_w,
            height: canvas_h,
            max_dim,
            fit,
            image: Arc::clone(&prepared),
        });

        Ok(prepared)
    }

    fn load_source(&self) -> Result<RgbaImage> {
        Ok(image::open(&self.path)
            .with_context(|| {
                format!(
                    "failed to load fixed background image at {}",
                    self.path.display()
                )
            })?
            .to_rgba8())
    }
}
