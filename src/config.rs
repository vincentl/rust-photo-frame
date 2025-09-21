use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use serde::Deserialize;

use image::RgbaImage;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MattingOptions {
    #[serde(default = "MattingOptions::default_minimum_percentage")]
    pub minimum_mat_percentage: f32,
    #[serde(default = "MattingOptions::default_max_upscale_factor")]
    pub max_upscale_factor: f32,
    #[serde(default, flatten)]
    pub style: MattingMode,
    #[serde(default, skip_deserializing)]
    pub runtime: MattingRuntime,
}

#[derive(Debug, Clone, Default)]
pub struct MattingRuntime {
    pub fixed_image: Option<Arc<RgbaImage>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum MattingMode {
    #[serde(rename = "fixed-color")]
    FixedColor {
        #[serde(default = "MattingMode::default_color")]
        color: [u8; 3],
    },
    Blur {
        #[serde(default = "MattingMode::default_blur_sigma")]
        sigma: f32,
        #[serde(default, rename = "max-sample-dim")]
        max_sample_dim: Option<u32>,
        #[serde(default)]
        backend: BlurBackend,
    },
    Studio {
        #[serde(
            default = "MattingMode::default_studio_bevel_width_px",
            rename = "bevel-width-px"
        )]
        bevel_width_px: f32,
        #[serde(
            default = "MattingMode::default_studio_bevel_color",
            rename = "bevel-color"
        )]
        bevel_color: [u8; 3],
    },
    FixedImage {
        path: PathBuf,
        #[serde(default)]
        fit: FixedImageFit,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlurBackend {
    Cpu,
    Neon,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixedImageFit {
    Cover,
    Contain,
    Stretch,
}

impl Default for BlurBackend {
    fn default() -> Self {
        Self::Cpu
    }
}

impl Default for FixedImageFit {
    fn default() -> Self {
        Self::Cover
    }
}

impl Default for MattingOptions {
    fn default() -> Self {
        Self {
            minimum_mat_percentage: Self::default_minimum_percentage(),
            max_upscale_factor: Self::default_max_upscale_factor(),
            style: MattingMode::default(),
            runtime: MattingRuntime::default(),
        }
    }
}

impl MattingOptions {
    const fn default_minimum_percentage() -> f32 {
        0.0
    }

    const fn default_max_upscale_factor() -> f32 {
        1.0
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        self.runtime = MattingRuntime::default();
        match &self.style {
            MattingMode::FixedImage { path, .. } => {
                let img = image::open(path)
                    .with_context(|| {
                        format!(
                            "failed to load fixed background image at {}",
                            path.display()
                        )
                    })?
                    .to_rgba8();
                self.runtime.fixed_image = Some(Arc::new(img));
            }
            _ => {}
        }
        Ok(())
    }
}

impl Default for MattingMode {
    fn default() -> Self {
        Self::FixedColor {
            color: Self::default_color(),
        }
    }
}

impl MattingMode {
    const fn default_color() -> [u8; 3] {
        [0, 0, 0]
    }

    const fn default_blur_sigma() -> f32 {
        20.0
    }

    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    pub const fn default_blur_max_sample_dim() -> u32 {
        2048
    }

    const fn default_studio_bevel_width_px() -> f32 {
        3.0
    }

    const fn default_studio_bevel_color() -> [u8; 3] {
        [255, 255, 255]
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Configuration {
    /// Root directory to scan recursively for images.
    pub photo_library_path: PathBuf,
    /// GPU render oversample factor relative to screen size (1.0 = native).
    pub oversample: f32,
    /// Cross-fade duration in milliseconds.
    pub fade_ms: u64,
    /// Time an image remains fully visible before starting a transition, in ms.
    pub dwell_ms: u64,
    /// How many images the viewer preloads/keeps pending.
    pub viewer_preload_count: usize,
    /// Maximum number of concurrent image decodes in the loader.
    pub loader_max_concurrent_decodes: usize,
    /// Optional deterministic seed for initial photo shuffle.
    pub startup_shuffle_seed: Option<u64>,
    /// Matting configuration for displayed photos.
    pub matting: MattingOptions,
}

impl Configuration {
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&s)?)
    }

    /// Validate runtime invariants that cannot be expressed via serde defaults alone.
    pub fn validated(mut self) -> Result<Self> {
        ensure!(
            self.viewer_preload_count > 0,
            "viewer-preload-count must be greater than zero"
        );
        ensure!(
            self.loader_max_concurrent_decodes > 0,
            "loader-max-concurrent-decodes must be greater than zero"
        );
        ensure!(self.oversample > 0.0, "oversample must be positive");
        ensure!(self.fade_ms > 0, "fade-ms must be greater than zero");
        ensure!(self.dwell_ms > 0, "dwell-ms must be greater than zero");
        self.matting.max_upscale_factor = self
            .matting
            .max_upscale_factor
            .max(MattingOptions::default_max_upscale_factor());
        self.matting
            .prepare_runtime()
            .context("failed to prepare matting resources")?;
        Ok(self)
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            photo_library_path: PathBuf::new(),
            oversample: 1.0,
            fade_ms: 400,
            dwell_ms: 2000,
            viewer_preload_count: 3,
            loader_max_concurrent_decodes: 4,
            startup_shuffle_seed: None,
            matting: MattingOptions::default(),
        }
    }
}
