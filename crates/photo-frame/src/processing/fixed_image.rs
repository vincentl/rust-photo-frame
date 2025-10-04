use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::anyhow;

use anyhow::{Context, Result};
use fast_image_resize as fir;
use image::{imageops, ImageFormat, ImageReader, RgbaImage};
use jpeg_decoder::{Decoder as JpegDecoder, PixelFormat};
use tracing::debug;

use crate::config::FixedImageFit;
use crate::processing::color::average_color_rgba;
use crate::processing::layout::{center_offset, resize_to_contain, resize_to_cover};

#[derive(Debug)]
pub struct FixedImageBackground {
    path: PathBuf,
    format: Option<ImageFormat>,
    source_width: u32,
    source_height: u32,
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

        let reader = ImageReader::open(&path).with_context(|| {
            format!(
                "failed to open fixed background image at {}",
                path.display()
            )
        })?;
        let reader = reader
            .with_guessed_format()
            .context("failed to guess fixed background format")?;
        let format = reader.format();
        let (source_width, source_height) = reader.into_dimensions().with_context(|| {
            format!(
                "failed to read image dimensions for fixed background at {}",
                path.display()
            )
        })?;

        Ok(Self {
            path,
            format,
            source_width,
            source_height,
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

        let decode_hint = match fit {
            FixedImageFit::Stretch => (canvas_w, canvas_h),
            FixedImageFit::Cover => resize_to_cover(
                canvas_w,
                canvas_h,
                self.source_width,
                self.source_height,
                max_dim,
            ),
            FixedImageFit::Contain => resize_to_contain(
                canvas_w,
                canvas_h,
                self.source_width,
                self.source_height,
                max_dim,
            ),
        };

        let source = self.load_source(decode_hint.0.max(1), decode_hint.1.max(1))?;
        let avg = *self
            .average_color
            .get_or_init(|| average_color_rgba(&source));

        let prepared = match fit {
            FixedImageFit::Stretch => resize_rgba(&source, canvas_w, canvas_h)?,
            FixedImageFit::Cover => {
                let (bg_w, bg_h) =
                    resize_to_cover(canvas_w, canvas_h, source.width(), source.height(), max_dim);
                let resized = resize_rgba(&source, bg_w, bg_h)?;
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
                let resized = resize_rgba(&source, bg_w, bg_h)?;
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

    fn load_source(&self, target_w: u32, target_h: u32) -> Result<RgbaImage> {
        if matches!(self.format, Some(ImageFormat::Jpeg)) {
            match self.decode_jpeg_scaled(target_w, target_h) {
                Ok(img) => return Ok(img),
                Err(err) => {
                    debug!(
                        "failed to decode JPEG {:?} with scaled fast path: {err:#} — falling back",
                        self.path
                    );
                }
            }
        }

        Ok(image::open(&self.path)
            .with_context(|| {
                format!(
                    "failed to load fixed background image at {}",
                    self.path.display()
                )
            })?
            .to_rgba8())
    }

    fn decode_jpeg_scaled(&self, target_w: u32, target_h: u32) -> Result<RgbaImage> {
        use jpeg_decoder::Error as JpegError;

        let file = fs::File::open(&self.path).with_context(|| {
            format!(
                "failed to open fixed background image at {}",
                self.path.display()
            )
        })?;
        let mut decoder = JpegDecoder::new(BufReader::new(file));
        let request_w = target_w.clamp(1, u16::MAX as u32) as u16;
        let request_h = target_h.clamp(1, u16::MAX as u32) as u16;
        decoder
            .scale(request_w, request_h)
            .map_err(|err| match err {
                JpegError::Unsupported(feature) => anyhow!("unsupported JPEG feature: {feature:?}"),
                other => anyhow!(other),
            })?;
        let pixels = decoder.decode().map_err(|err| match err {
            JpegError::Unsupported(feature) => anyhow!("unsupported JPEG feature: {feature:?}"),
            other => anyhow!(other),
        })?;
        let info = decoder.info().ok_or_else(|| {
            anyhow!(
                "missing image info while decoding JPEG {}",
                self.path.display()
            )
        })?;
        let width = info.width as u32;
        let height = info.height as u32;

        let rgba = match info.pixel_format {
            PixelFormat::RGB24 => {
                let mut rgba = Vec::with_capacity(pixels.len() / 3 * 4);
                for chunk in pixels.chunks_exact(3) {
                    rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
                }
                rgba
            }
            PixelFormat::L8 => {
                let mut rgba = Vec::with_capacity(pixels.len() * 4);
                for &v in &pixels {
                    rgba.extend_from_slice(&[v, v, v, 255]);
                }
                rgba
            }
            PixelFormat::CMYK32 => {
                let mut rgba = Vec::with_capacity(pixels.len() / 4 * 4);
                for chunk in pixels.chunks_exact(4) {
                    let c = chunk[0] as f32 / 255.0;
                    let m = chunk[1] as f32 / 255.0;
                    let y = chunk[2] as f32 / 255.0;
                    let k = chunk[3] as f32 / 255.0;
                    let c = c * (1.0 - k) + k;
                    let m = m * (1.0 - k) + k;
                    let y = y * (1.0 - k) + k;
                    let r = ((1.0 - c) * 255.0).round().clamp(0.0, 255.0) as u8;
                    let g = ((1.0 - m) * 255.0).round().clamp(0.0, 255.0) as u8;
                    let b = ((1.0 - y) * 255.0).round().clamp(0.0, 255.0) as u8;
                    rgba.extend_from_slice(&[r, g, b, 255]);
                }
                rgba
            }
            PixelFormat::L16 => {
                anyhow::bail!("16-bit grayscale JPEGs are not supported by the scaled decoder");
            }
        };

        RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| anyhow!("failed to construct RGBA image for {}", self.path.display()))
    }
}

fn resize_rgba(source: &RgbaImage, target_w: u32, target_h: u32) -> Result<RgbaImage> {
    if target_w == 0 || target_h == 0 {
        anyhow::bail!("resize dimensions must be positive");
    }
    if source.width() == target_w && source.height() == target_h {
        return Ok(source.clone());
    }

    let src_view = fir::images::ImageRef::new(
        source.width(),
        source.height(),
        source.as_raw(),
        fir::PixelType::U8x4,
    )
    .context("failed to create source view for fixed-image resize")?;
    let mut dst_image = fir::images::Image::new(target_w, target_h, fir::PixelType::U8x4);
    let options = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom));
    let mut resizer = fir::Resizer::new();
    resizer
        .resize(&src_view, &mut dst_image, Some(&options))
        .context("fixed-image resize failed")?;
    let buffer = dst_image.into_vec();
    RgbaImage::from_raw(target_w, target_h, buffer)
        .ok_or_else(|| anyhow::anyhow!("failed to construct resized RGBA image"))
}
