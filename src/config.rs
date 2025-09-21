use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use serde::de::{Error as DeError, IntoDeserializer};
use serde::Deserialize;

use image::RgbaImage;
use serde_yaml::Value as YamlValue;

#[derive(Debug, Clone)]
pub struct MattingOptions {
    pub minimum_mat_percentage: f32,
    pub max_upscale_factor: f32,
    pub style: MattingMode,
    pub runtime: MattingRuntime,
}

#[derive(Debug, Clone, Default)]
pub struct MattingRuntime {
    pub fixed_image: Option<Arc<RgbaImage>>,
}

#[derive(Debug, Clone)]
pub enum MattingMode {
    FixedColor {
        color: [u8; 3],
    },
    Blur {
        sigma: f32,
        max_sample_dim: Option<u32>,
        backend: BlurBackend,
    },
    Studio {
        bevel_width_px: f32,
        highlight_strength: f32,
        shadow_strength: f32,
        bevel_angle_deg: f32,
        linen_intensity: f32,
        linen_scale_px: f32,
        linen_rotation_deg: f32,
        light_dir: [f32; 3],
        shadow_radius_px: f32,
        shadow_offset_px: f32,
    },
    FixedImage {
        path: PathBuf,
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

impl<'de> Deserialize<'de> for MattingOptions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut value = YamlValue::deserialize(deserializer)?;
        let mapping = value
            .as_mapping_mut()
            .ok_or_else(|| DeError::custom("matting: expected a mapping"))?;

        let minimum_mat_percentage =
            match mapping.remove(&YamlValue::String("minimum-mat-percentage".to_string())) {
                Some(raw) => serde_yaml::from_value(raw)
                    .map_err(|err| DeError::custom(format!("matting: {err}")))?,
                None => MattingOptions::default_minimum_percentage(),
            };

        let max_upscale_factor =
            match mapping.remove(&YamlValue::String("max-upscale-factor".to_string())) {
                Some(raw) => serde_yaml::from_value::<f32>(raw)
                    .map(|v| v.max(1.0))
                    .map_err(|err| DeError::custom(format!("matting: {err}")))?,
                None => MattingOptions::default_max_upscale_factor(),
            };

        let style = if mapping.is_empty() {
            MattingMode::default()
        } else {
            if !mapping.contains_key(&YamlValue::String("type".to_string())) {
                return Err(DeError::custom(
                    "matting: missing `type` while additional fields were provided",
                ));
            }

            MattingMode::deserialize(
                YamlValue::Mapping(std::mem::take(mapping)).into_deserializer(),
            )
            .map_err(|err| DeError::custom(format!("matting: {err}")))?
        };

        Ok(Self {
            minimum_mat_percentage,
            max_upscale_factor,
            style,
            runtime: MattingRuntime::default(),
        })
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

impl<'de> Deserialize<'de> for MattingMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct FixedColorFields {
            #[serde(default = "MattingMode::default_color")]
            color: [u8; 3],
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct BlurFields {
            #[serde(default = "MattingMode::default_blur_sigma")]
            sigma: f32,
            #[serde(default)]
            max_sample_dim: Option<u32>,
            #[serde(default)]
            backend: BlurBackend,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct StudioFields {
            #[serde(default = "MattingMode::default_studio_bevel_width_px")]
            bevel_width_px: f32,
            #[serde(default = "MattingMode::default_studio_highlight_strength")]
            highlight_strength: f32,
            #[serde(default = "MattingMode::default_studio_shadow_strength")]
            shadow_strength: f32,
            #[serde(default = "MattingMode::default_studio_bevel_angle")]
            bevel_angle_deg: f32,
            #[serde(default = "MattingMode::default_studio_linen_intensity")]
            linen_intensity: f32,
            #[serde(default = "MattingMode::default_studio_linen_scale")]
            linen_scale_px: f32,
            #[serde(default = "MattingMode::default_studio_linen_rotation")]
            linen_rotation_deg: f32,
            #[serde(default = "MattingMode::default_studio_light_dir")]
            light_dir: [f32; 3],
            #[serde(default = "MattingMode::default_studio_shadow_radius")]
            shadow_radius_px: f32,
            #[serde(default = "MattingMode::default_studio_shadow_offset")]
            shadow_offset_px: f32,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct FixedImageFields {
            path: PathBuf,
            #[serde(default)]
            fit: FixedImageFit,
        }

        let mut value = YamlValue::deserialize(deserializer)?;
        let mapping = value
            .as_mapping_mut()
            .ok_or_else(|| DeError::custom("matting: expected a mapping"))?;

        let mode_value = mapping
            .remove(&YamlValue::String("type".to_string()))
            .ok_or_else(|| DeError::custom("matting: missing `type` field"))?;

        let mode = match mode_value {
            YamlValue::String(name) => name,
            other => {
                return Err(DeError::custom(format!(
                    "matting: `type` must be a string, got {other:?}"
                )))
            }
        };

        match mode.as_str() {
            "fixed-color" => {
                let payload = YamlValue::Mapping(std::mem::take(mapping));
                let fields = FixedColorFields::deserialize(payload.into_deserializer())
                    .map_err(|err| DeError::custom(format!("matting: {err}")))?;
                Ok(Self::FixedColor {
                    color: fields.color,
                })
            }
            "blur" => {
                let payload = YamlValue::Mapping(std::mem::take(mapping));
                let fields = BlurFields::deserialize(payload.into_deserializer())
                    .map_err(|err| DeError::custom(format!("matting: {err}")))?;
                Ok(Self::Blur {
                    sigma: fields.sigma,
                    max_sample_dim: fields.max_sample_dim,
                    backend: fields.backend,
                })
            }
            "studio" => {
                let payload = YamlValue::Mapping(std::mem::take(mapping));
                let fields = StudioFields::deserialize(payload.into_deserializer())
                    .map_err(|err| DeError::custom(format!("matting: {err}")))?;
                Ok(Self::Studio {
                    bevel_width_px: fields.bevel_width_px,
                    highlight_strength: fields.highlight_strength,
                    shadow_strength: fields.shadow_strength,
                    bevel_angle_deg: fields.bevel_angle_deg,
                    linen_intensity: fields.linen_intensity,
                    linen_scale_px: fields.linen_scale_px,
                    linen_rotation_deg: fields.linen_rotation_deg,
                    light_dir: fields.light_dir,
                    shadow_radius_px: fields.shadow_radius_px,
                    shadow_offset_px: fields.shadow_offset_px,
                })
            }
            "fixed-image" => {
                let payload = YamlValue::Mapping(std::mem::take(mapping));
                let fields = FixedImageFields::deserialize(payload.into_deserializer())
                    .map_err(|err| DeError::custom(format!("matting: {err}")))?;
                Ok(Self::FixedImage {
                    path: fields.path,
                    fit: fields.fit,
                })
            }
            other => Err(DeError::custom(format!(
                "matting: unknown matting `type` `{}`",
                other
            ))),
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

    const fn default_studio_highlight_strength() -> f32 {
        1.0
    }

    const fn default_studio_shadow_strength() -> f32 {
        1.0
    }

    const fn default_studio_bevel_angle() -> f32 {
        45.0
    }

    const fn default_studio_linen_intensity() -> f32 {
        0.7
    }

    const fn default_studio_linen_scale() -> f32 {
        900.0
    }

    const fn default_studio_linen_rotation() -> f32 {
        12.0
    }

    const fn default_studio_light_dir() -> [f32; 3] {
        [-0.6, -0.8, 0.2]
    }

    const fn default_studio_shadow_radius() -> f32 {
        1.0
    }

    const fn default_studio_shadow_offset() -> f32 {
        0.75
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
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
