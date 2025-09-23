use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{ensure, Context, Result};
use rand::seq::IteratorRandom;
use rand::Rng;
use serde::de::{self, DeserializeOwned, DeserializeSeed, Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use serde_yaml::Value as YamlValue;

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

#[derive(Debug, Clone)]
pub struct MattingConfig {
    selection: MattingSelection,
    options: BTreeMap<MattingKind, MattingOptions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MattingSelection {
    Fixed(MattingKind),
    Random,
}

#[derive(Debug, Clone, Default)]
pub struct MattingRuntime {
    pub fixed_image: Option<Arc<RgbaImage>>,
}

impl MattingKind {
    const ALL: &'static [Self] = &[Self::FixedColor, Self::Blur, Self::Studio, Self::FixedImage];
    const NAMES: &'static [&'static str] = &["fixed-color", "blur", "studio", "fixed-image"];

    fn as_str(&self) -> &'static str {
        match self {
            Self::FixedColor => "fixed-color",
            Self::Blur => "blur",
            Self::Studio => "studio",
            Self::FixedImage => "fixed-image",
        }
    }
}

impl fmt::Display for MattingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MattingKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        for kind in Self::ALL {
            if raw == kind.as_str() {
                return Ok(*kind);
            }
        }
        Err(de::Error::unknown_variant(&raw, Self::NAMES))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MattingKind {
    FixedColor,
    Blur,
    Studio,
    FixedImage,
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
        #[serde(
            default = "MattingMode::default_studio_texture_strength",
            rename = "texture-strength"
        )]
        texture_strength: f32,
        #[serde(
            default = "MattingMode::default_studio_warp_period_px",
            rename = "warp-period-px"
        )]
        warp_period_px: f32,
        #[serde(
            default = "MattingMode::default_studio_weft_period_px",
            rename = "weft-period-px"
        )]
        weft_period_px: f32,
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
        self.max_upscale_factor = self
            .max_upscale_factor
            .max(Self::default_max_upscale_factor());
        self.runtime = MattingRuntime::default();
        if let MattingMode::FixedImage { path, .. } = &self.style {
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
        Ok(())
    }

    pub fn fixed_color(&self) -> Option<[u8; 3]> {
        match self.style {
            MattingMode::FixedColor { color } => Some(color),
            _ => None,
        }
    }
}

impl MattingMode {
    fn kind(&self) -> MattingKind {
        match self {
            MattingMode::FixedColor { .. } => MattingKind::FixedColor,
            MattingMode::Blur { .. } => MattingKind::Blur,
            MattingMode::Studio { .. } => MattingKind::Studio,
            MattingMode::FixedImage { .. } => MattingKind::FixedImage,
        }
    }
}

impl MattingOptions {
    fn with_kind(kind: MattingKind, base: MattingOptionBuilder) -> Self {
        let style = match kind {
            MattingKind::FixedColor => MattingMode::FixedColor {
                color: base.color.unwrap_or_else(MattingMode::default_color),
            },
            MattingKind::Blur => MattingMode::Blur {
                sigma: base.sigma.unwrap_or_else(MattingMode::default_blur_sigma),
                max_sample_dim: base.max_sample_dim,
                backend: base.blur_backend.unwrap_or_default(),
            },
            MattingKind::Studio => MattingMode::Studio {
                bevel_width_px: base
                    .bevel_width_px
                    .unwrap_or_else(MattingMode::default_studio_bevel_width_px),
                bevel_color: base
                    .bevel_color
                    .unwrap_or_else(MattingMode::default_studio_bevel_color),
                texture_strength: base
                    .texture_strength
                    .unwrap_or_else(MattingMode::default_studio_texture_strength),
                warp_period_px: base
                    .warp_period_px
                    .unwrap_or_else(MattingMode::default_studio_warp_period_px),
                weft_period_px: base
                    .weft_period_px
                    .unwrap_or_else(MattingMode::default_studio_weft_period_px),
            },
            MattingKind::FixedImage => MattingMode::FixedImage {
                path: base.path.expect("fixed-image matting must supply a path"),
                fit: base.fixed_image_fit.unwrap_or_default(),
            },
        };
        Self {
            minimum_mat_percentage: base
                .minimum_mat_percentage
                .unwrap_or_else(Self::default_minimum_percentage),
            max_upscale_factor: base
                .max_upscale_factor
                .unwrap_or_else(Self::default_max_upscale_factor),
            style,
            runtime: MattingRuntime::default(),
        }
    }

    fn kind(&self) -> MattingKind {
        self.style.kind()
    }
}

#[derive(Default)]
struct MattingOptionBuilder {
    minimum_mat_percentage: Option<f32>,
    max_upscale_factor: Option<f32>,
    color: Option<[u8; 3]>,
    sigma: Option<f32>,
    max_sample_dim: Option<u32>,
    blur_backend: Option<BlurBackend>,
    bevel_width_px: Option<f32>,
    bevel_color: Option<[u8; 3]>,
    texture_strength: Option<f32>,
    warp_period_px: Option<f32>,
    weft_period_px: Option<f32>,
    path: Option<PathBuf>,
    fixed_image_fit: Option<FixedImageFit>,
}

fn inline_value_to<T, E>(value: YamlValue) -> Result<T, E>
where
    T: DeserializeOwned,
    E: de::Error,
{
    serde_yaml::from_value(value).map_err(|err| de::Error::custom(err.to_string()))
}

fn apply_inline_field<E>(
    builder: &mut MattingOptionBuilder,
    kind: MattingKind,
    key: &str,
    value: YamlValue,
) -> Result<(), E>
where
    E: de::Error,
{
    match key {
        "minimum-mat-percentage" => {
            if builder.minimum_mat_percentage.is_some() {
                return Err(de::Error::duplicate_field("minimum-mat-percentage"));
            }
            builder.minimum_mat_percentage = Some(inline_value_to::<f32, E>(value)?);
        }
        "max-upscale-factor" => {
            if builder.max_upscale_factor.is_some() {
                return Err(de::Error::duplicate_field("max-upscale-factor"));
            }
            builder.max_upscale_factor = Some(inline_value_to::<f32, E>(value)?);
        }
        other => match kind {
            MattingKind::FixedColor => match other {
                "color" => {
                    if builder.color.is_some() {
                        return Err(de::Error::duplicate_field("color"));
                    }
                    builder.color = Some(inline_value_to::<[u8; 3], E>(value)?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        other,
                        &["color", "minimum-mat-percentage", "max-upscale-factor"],
                    ));
                }
            },
            MattingKind::Blur => match other {
                "sigma" => {
                    if builder.sigma.is_some() {
                        return Err(de::Error::duplicate_field("sigma"));
                    }
                    builder.sigma = Some(inline_value_to::<f32, E>(value)?);
                }
                "max-sample-dim" => {
                    if builder.max_sample_dim.is_some() {
                        return Err(de::Error::duplicate_field("max-sample-dim"));
                    }
                    builder.max_sample_dim = Some(inline_value_to::<u32, E>(value)?);
                }
                "backend" => {
                    if builder.blur_backend.is_some() {
                        return Err(de::Error::duplicate_field("backend"));
                    }
                    builder.blur_backend = Some(inline_value_to::<BlurBackend, E>(value)?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        other,
                        &[
                            "sigma",
                            "max-sample-dim",
                            "backend",
                            "minimum-mat-percentage",
                            "max-upscale-factor",
                        ],
                    ));
                }
            },
            MattingKind::Studio => match other {
                "bevel-width-px" => {
                    if builder.bevel_width_px.is_some() {
                        return Err(de::Error::duplicate_field("bevel-width-px"));
                    }
                    builder.bevel_width_px = Some(inline_value_to::<f32, E>(value)?);
                }
                "bevel-color" => {
                    if builder.bevel_color.is_some() {
                        return Err(de::Error::duplicate_field("bevel-color"));
                    }
                    builder.bevel_color = Some(inline_value_to::<[u8; 3], E>(value)?);
                }
                "texture-strength" => {
                    if builder.texture_strength.is_some() {
                        return Err(de::Error::duplicate_field("texture-strength"));
                    }
                    builder.texture_strength = Some(inline_value_to::<f32, E>(value)?);
                }
                "warp-period-px" => {
                    if builder.warp_period_px.is_some() {
                        return Err(de::Error::duplicate_field("warp-period-px"));
                    }
                    builder.warp_period_px = Some(inline_value_to::<f32, E>(value)?);
                }
                "weft-period-px" => {
                    if builder.weft_period_px.is_some() {
                        return Err(de::Error::duplicate_field("weft-period-px"));
                    }
                    builder.weft_period_px = Some(inline_value_to::<f32, E>(value)?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        other,
                        &[
                            "bevel-width-px",
                            "bevel-color",
                            "texture-strength",
                            "warp-period-px",
                            "weft-period-px",
                            "minimum-mat-percentage",
                            "max-upscale-factor",
                        ],
                    ));
                }
            },
            MattingKind::FixedImage => match other {
                "path" => {
                    if builder.path.is_some() {
                        return Err(de::Error::duplicate_field("path"));
                    }
                    builder.path = Some(inline_value_to::<PathBuf, E>(value)?);
                }
                "fit" => {
                    if builder.fixed_image_fit.is_some() {
                        return Err(de::Error::duplicate_field("fit"));
                    }
                    builder.fixed_image_fit = Some(inline_value_to::<FixedImageFit, E>(value)?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        other,
                        &[
                            "path",
                            "fit",
                            "minimum-mat-percentage",
                            "max-upscale-factor",
                        ],
                    ));
                }
            },
        },
    }
    Ok(())
}

impl Default for MattingConfig {
    fn default() -> Self {
        let mut options = BTreeMap::new();
        options.insert(MattingKind::FixedColor, MattingOptions::default());
        Self {
            selection: MattingSelection::Fixed(MattingKind::FixedColor),
            options,
        }
    }
}

impl<'de> Deserialize<'de> for MattingConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(MattingConfigVisitor)
    }
}

struct MattingConfigVisitor;

impl<'de> Visitor<'de> for MattingConfigVisitor {
    type Value = MattingConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a matting configuration map")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut selection: Option<MattingSelection> = None;
        let mut options: Option<BTreeMap<MattingKind, MattingOptions>> = None;
        let mut inline_fields: Vec<(String, YamlValue)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => {
                    if selection.is_some() {
                        return Err(de::Error::duplicate_field("type"));
                    }
                    let raw: String = map.next_value()?;
                    selection = Some(parse_matting_selection(&raw).map_err(de::Error::custom)?);
                }
                "options" => {
                    if options.is_some() {
                        return Err(de::Error::duplicate_field("options"));
                    }
                    options = Some(map.next_value_seed(MattingOptionsMapSeed)?);
                }
                _ => {
                    let value = map.next_value::<YamlValue>()?;
                    inline_fields.push((key, value));
                }
            }
        }

        let mut options = options.unwrap_or_default();

        let selection = selection.ok_or_else(|| de::Error::missing_field("type"))?;

        match selection {
            MattingSelection::Random => {
                if !inline_fields.is_empty() {
                    return Err(de::Error::custom(
                        "matting.type random does not support inline matting fields",
                    ));
                }
            }
            MattingSelection::Fixed(kind) => {
                if options.is_empty() {
                    let mut builder = MattingOptionBuilder::default();
                    for (field, value) in std::mem::take(&mut inline_fields) {
                        apply_inline_field::<A::Error>(&mut builder, kind, &field, value)?;
                    }
                    if matches!(kind, MattingKind::FixedImage) && builder.path.is_none() {
                        return Err(de::Error::missing_field("path"));
                    }
                    let option = MattingOptions::with_kind(kind, builder);
                    options.insert(kind, option);
                } else {
                    if !inline_fields.is_empty() {
                        return Err(de::Error::custom(
                            "matting configuration cannot mix inline fields with matting.options",
                        ));
                    }
                    if !options.contains_key(&kind) {
                        return Err(de::Error::custom(format!(
                            "matting.type {} must match a key in matting.options",
                            kind
                        )));
                    }
                }
            }
        }

        if options.is_empty() {
            return Err(de::Error::custom(
                "matting.options must include at least one entry",
            ));
        }

        Ok(MattingConfig { selection, options })
    }
}

fn parse_matting_selection(raw: &str) -> Result<MattingSelection> {
    if raw == "random" {
        return Ok(MattingSelection::Random);
    }
    for kind in MattingKind::ALL {
        if raw == kind.as_str() {
            return Ok(MattingSelection::Fixed(*kind));
        }
    }
    Err(anyhow::anyhow!(
        "unknown matting type '{}', expected one of: random, {}",
        raw,
        MattingKind::NAMES.join(", ")
    ))
}

struct MattingOptionsMapSeed;

impl<'de> DeserializeSeed<'de> for MattingOptionsMapSeed {
    type Value = BTreeMap<MattingKind, MattingOptions>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(MattingOptionsMapVisitor)
    }
}

struct MattingOptionsMapVisitor;

impl<'de> Visitor<'de> for MattingOptionsMapVisitor {
    type Value = BTreeMap<MattingKind, MattingOptions>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map of matting options keyed by matting type")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut options = BTreeMap::new();
        while let Some(kind) = map.next_key::<MattingKind>()? {
            if options.contains_key(&kind) {
                return Err(de::Error::custom(format!(
                    "duplicate matting option for type {}",
                    kind
                )));
            }
            let option = map.next_value_seed(MattingOptionSeed { kind })?;
            if option.kind() != kind {
                return Err(de::Error::custom(format!(
                    "matting option for key {} does not match its configuration",
                    kind
                )));
            }
            options.insert(kind, option);
        }
        Ok(options)
    }
}

struct MattingOptionSeed {
    kind: MattingKind,
}

impl<'de> DeserializeSeed<'de> for MattingOptionSeed {
    type Value = MattingOptions;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(MattingOptionVisitor { kind: self.kind })
    }
}

struct MattingOptionVisitor {
    kind: MattingKind,
}

impl<'de> Visitor<'de> for MattingOptionVisitor {
    type Value = MattingOptions;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a matting option configuration map")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut builder = MattingOptionBuilder::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "minimum-mat-percentage" => {
                    if builder.minimum_mat_percentage.is_some() {
                        return Err(de::Error::duplicate_field("minimum-mat-percentage"));
                    }
                    builder.minimum_mat_percentage = Some(map.next_value()?);
                }
                "max-upscale-factor" => {
                    if builder.max_upscale_factor.is_some() {
                        return Err(de::Error::duplicate_field("max-upscale-factor"));
                    }
                    builder.max_upscale_factor = Some(map.next_value()?);
                }
                other => match self.kind {
                    MattingKind::FixedColor => match other {
                        "color" => {
                            if builder.color.is_some() {
                                return Err(de::Error::duplicate_field("color"));
                            }
                            builder.color = Some(map.next_value()?);
                        }
                        _ => {
                            return Err(de::Error::unknown_field(
                                other,
                                &["color", "minimum-mat-percentage", "max-upscale-factor"],
                            ))
                        }
                    },
                    MattingKind::Blur => match other {
                        "sigma" => {
                            if builder.sigma.is_some() {
                                return Err(de::Error::duplicate_field("sigma"));
                            }
                            builder.sigma = Some(map.next_value()?);
                        }
                        "max-sample-dim" => {
                            if builder.max_sample_dim.is_some() {
                                return Err(de::Error::duplicate_field("max-sample-dim"));
                            }
                            builder.max_sample_dim = Some(map.next_value()?);
                        }
                        "backend" => {
                            if builder.blur_backend.is_some() {
                                return Err(de::Error::duplicate_field("backend"));
                            }
                            builder.blur_backend = Some(map.next_value()?);
                        }
                        _ => {
                            return Err(de::Error::unknown_field(
                                other,
                                &[
                                    "sigma",
                                    "max-sample-dim",
                                    "backend",
                                    "minimum-mat-percentage",
                                    "max-upscale-factor",
                                ],
                            ))
                        }
                    },
                    MattingKind::Studio => match other {
                        "bevel-width-px" => {
                            if builder.bevel_width_px.is_some() {
                                return Err(de::Error::duplicate_field("bevel-width-px"));
                            }
                            builder.bevel_width_px = Some(map.next_value()?);
                        }
                        "bevel-color" => {
                            if builder.bevel_color.is_some() {
                                return Err(de::Error::duplicate_field("bevel-color"));
                            }
                            builder.bevel_color = Some(map.next_value()?);
                        }
                        "texture-strength" => {
                            if builder.texture_strength.is_some() {
                                return Err(de::Error::duplicate_field("texture-strength"));
                            }
                            builder.texture_strength = Some(map.next_value()?);
                        }
                        "warp-period-px" => {
                            if builder.warp_period_px.is_some() {
                                return Err(de::Error::duplicate_field("warp-period-px"));
                            }
                            builder.warp_period_px = Some(map.next_value()?);
                        }
                        "weft-period-px" => {
                            if builder.weft_period_px.is_some() {
                                return Err(de::Error::duplicate_field("weft-period-px"));
                            }
                            builder.weft_period_px = Some(map.next_value()?);
                        }
                        _ => {
                            return Err(de::Error::unknown_field(
                                other,
                                &[
                                    "bevel-width-px",
                                    "bevel-color",
                                    "texture-strength",
                                    "warp-period-px",
                                    "weft-period-px",
                                    "minimum-mat-percentage",
                                    "max-upscale-factor",
                                ],
                            ))
                        }
                    },
                    MattingKind::FixedImage => match other {
                        "path" => {
                            if builder.path.is_some() {
                                return Err(de::Error::duplicate_field("path"));
                            }
                            builder.path = Some(map.next_value()?);
                        }
                        "fit" => {
                            if builder.fixed_image_fit.is_some() {
                                return Err(de::Error::duplicate_field("fit"));
                            }
                            builder.fixed_image_fit = Some(map.next_value()?);
                        }
                        _ => {
                            return Err(de::Error::unknown_field(
                                other,
                                &[
                                    "path",
                                    "fit",
                                    "minimum-mat-percentage",
                                    "max-upscale-factor",
                                ],
                            ))
                        }
                    },
                },
            }
        }

        if matches!(self.kind, MattingKind::FixedImage) && builder.path.is_none() {
            return Err(de::Error::missing_field("path"));
        }

        Ok(MattingOptions::with_kind(self.kind, builder))
    }
}

impl MattingConfig {
    #[allow(dead_code)]
    /// Exposed for integration tests to introspect the parsed selection strategy.
    pub fn selection(&self) -> MattingSelection {
        self.selection
    }

    #[allow(dead_code)]
    /// Exposed for integration tests to inspect the configured matting options.
    pub fn options(&self) -> &BTreeMap<MattingKind, MattingOptions> {
        &self.options
    }

    pub fn primary_option(&self) -> Option<&MattingOptions> {
        match self.selection {
            MattingSelection::Fixed(kind) => self.options.get(&kind),
            MattingSelection::Random => self.options.values().next(),
        }
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        ensure!(
            !self.options.is_empty(),
            "matting configuration must include at least one option"
        );
        for option in self.options.values_mut() {
            option
                .prepare_runtime()
                .context("failed to prepare matting resources")?;
        }
        Ok(())
    }

    pub fn choose_option<R: Rng + ?Sized>(&self, rng: &mut R) -> MattingOptions {
        match self.selection {
            MattingSelection::Fixed(kind) => self
                .options
                .get(&kind)
                .cloned()
                .expect("validated fixed matting should have selected option"),
            MattingSelection::Random => self
                .options
                .values()
                .choose(rng)
                .cloned()
                .expect("validated random matting should have options"),
        }
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

    const fn default_studio_texture_strength() -> f32 {
        1.0
    }

    const fn default_studio_warp_period_px() -> f32 {
        5.6
    }

    const fn default_studio_weft_period_px() -> f32 {
        5.2
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
    pub matting: MattingConfig,
    /// Playlist weighting options for how frequently new photos repeat.
    pub playlist: PlaylistOptions,
    /// Wi-Fi setup fallback flow for Raspberry Pi first boot scenarios.
    pub wifi_setup: WifiSetupConfig,
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
            .context("invalid matting configuration")?;
        self.playlist.validate()?;
        self.wifi_setup.validate()?;
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
            matting: MattingConfig::default(),
            playlist: PlaylistOptions::default(),
            wifi_setup: WifiSetupConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct WifiSetupConfig {
    /// Whether the application should perform Wi-Fi connectivity checks and host the setup portal.
    pub enabled: bool,
    /// Wireless interface name used for both hotspot and client connections.
    pub wifi_interface: String,
    /// Friendly SSID broadcast while in setup hotspot mode.
    pub hotspot_ssid: String,
    /// WPA2 password used for the setup hotspot.
    pub hotspot_password: String,
    /// Command (executable + args) that enables the hotspot on `wifi-interface`.
    pub hotspot_start_command: Vec<String>,
    /// Command (executable + args) that disables the hotspot on `wifi-interface`.
    pub hotspot_stop_command: Vec<String>,
    /// Address to bind the local setup HTTP server to.
    pub portal_bind_address: String,
    /// TCP port for the setup HTTP server.
    pub portal_port: u16,
    /// Hostname or IP displayed to the user for the setup portal.
    pub portal_hostname: String,
    /// Path to the wpa_supplicant configuration file that should receive new networks.
    pub wpa_supplicant_path: PathBuf,
    /// Optional override for invoking wpa_cli (defaults to `wpa_cli` in PATH).
    pub wpa_cli_path: Option<PathBuf>,
    /// Timeout in seconds for checking connectivity after credentials are applied.
    pub connection_check_timeout_secs: u64,
    /// Poll interval in seconds while waiting for the device to come online.
    pub connection_check_interval_secs: u64,
    /// Optional command to restart the photo frame service after a successful Wi-Fi join.
    pub restart_command: Vec<String>,
}

impl WifiSetupConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.hotspot_ssid.trim().is_empty(),
            "wifi-setup.hotspot-ssid must not be empty"
        );
        ensure!(
            !self.hotspot_password.trim().is_empty(),
            "wifi-setup.hotspot-password must not be empty"
        );
        ensure!(
            !self.portal_bind_address.trim().is_empty(),
            "wifi-setup.portal-bind-address must not be empty"
        );
        ensure!(
            !self.portal_hostname.trim().is_empty(),
            "wifi-setup.portal-hostname must not be empty"
        );
        ensure!(
            self.portal_port > 0,
            "wifi-setup.portal-port must be greater than zero"
        );
        ensure!(
            self.connection_check_timeout_secs >= self.connection_check_interval_secs,
            "wifi-setup.connection-check-timeout-secs must be >= connection-check-interval-secs"
        );
        Ok(())
    }

    pub fn portal_url(&self) -> String {
        format!("http://{}:{}", self.portal_hostname, self.portal_port)
    }
}

impl Default for WifiSetupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wifi_interface: "wlan0".to_string(),
            hotspot_ssid: "Frame Setup".to_string(),
            hotspot_password: "framesetup".to_string(),
            hotspot_start_command: vec![
                "sudo".to_string(),
                "systemctl".to_string(),
                "start".to_string(),
                "frame-hotspot.service".to_string(),
            ],
            hotspot_stop_command: vec![
                "sudo".to_string(),
                "systemctl".to_string(),
                "stop".to_string(),
                "frame-hotspot.service".to_string(),
            ],
            portal_bind_address: "0.0.0.0".to_string(),
            portal_port: 8080,
            portal_hostname: "10.0.0.1".to_string(),
            wpa_supplicant_path: PathBuf::from("/etc/wpa_supplicant/wpa_supplicant.conf"),
            wpa_cli_path: None,
            connection_check_timeout_secs: 45,
            connection_check_interval_secs: 3,
            restart_command: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PlaylistOptions {
    /// Initial multiplicity for brand new photos.
    pub new_multiplicity: u32,
    /// Half-life duration controlling the exponential decay of multiplicity.
    #[serde(with = "humantime_serde")]
    pub half_life: Duration,
}

impl PlaylistOptions {
    const fn default_new_multiplicity() -> u32 {
        3
    }

    const fn default_half_life() -> Duration {
        Duration::from_secs(60 * 60 * 24)
    }

    pub fn multiplicity_for(&self, created_at: SystemTime, now: SystemTime) -> usize {
        let age = now.duration_since(created_at).unwrap_or_default();
        let half_life = self.half_life.max(Duration::from_secs(1));
        let exponent = age.as_secs_f64() / half_life.as_secs_f64();
        let base = f64::from(self.new_multiplicity.max(1));
        let weight = base * 0.5_f64.powf(exponent);
        weight.ceil().max(1.0) as usize
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.new_multiplicity >= 1,
            "playlist.new-multiplicity must be >= 1"
        );
        ensure!(
            self.half_life > Duration::from_secs(0),
            "playlist.half-life must be positive"
        );
        Ok(())
    }
}

impl Default for PlaylistOptions {
    fn default() -> Self {
        Self {
            new_multiplicity: Self::default_new_multiplicity(),
            half_life: Self::default_half_life(),
        }
    }
}
