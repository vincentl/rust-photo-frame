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

use crate::processing::fixed_image::FixedImageBackground;

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
    pub fixed_image: Option<Arc<FixedImageBackground>>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
            let background = FixedImageBackground::new(path.clone()).with_context(|| {
                format!(
                    "failed to prepare fixed background image at {}",
                    path.display()
                )
            })?;
            self.runtime.fixed_image = Some(Arc::new(background));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionSelection {
    Fixed(TransitionKind),
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransitionKind {
    Fade,
    Wipe,
    Push,
}

impl TransitionKind {
    const ALL: &'static [Self] = &[Self::Fade, Self::Wipe, Self::Push];
    const NAMES: &'static [&'static str] = &["fade", "wipe", "push"];

    fn as_str(&self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::Wipe => "wipe",
            Self::Push => "push",
        }
    }

    pub const fn as_index(&self) -> u32 {
        match self {
            Self::Fade => 1,
            Self::Wipe => 2,
            Self::Push => 3,
        }
    }
}

impl fmt::Display for TransitionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TransitionKind {
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

#[derive(Debug, Clone)]
pub struct TransitionConfig {
    selection: TransitionSelection,
    options: BTreeMap<TransitionKind, TransitionOptions>,
}

impl Default for TransitionConfig {
    fn default() -> Self {
        let mut options = BTreeMap::new();
        options.insert(
            TransitionKind::Fade,
            TransitionOptions::default_for(TransitionKind::Fade),
        );
        Self {
            selection: TransitionSelection::Fixed(TransitionKind::Fade),
            options,
        }
    }
}

impl TransitionConfig {
    #[allow(dead_code)]
    pub fn selection(&self) -> TransitionSelection {
        self.selection
    }

    #[allow(dead_code)]
    pub fn options(&self) -> &BTreeMap<TransitionKind, TransitionOptions> {
        &self.options
    }

    #[allow(dead_code)]
    pub fn primary_option(&self) -> Option<&TransitionOptions> {
        match self.selection {
            TransitionSelection::Fixed(kind) => self.options.get(&kind),
            TransitionSelection::Random => self.options.values().next(),
        }
    }

    pub fn choose_option<R: Rng + ?Sized>(&self, rng: &mut R) -> TransitionOptions {
        match self.selection {
            TransitionSelection::Fixed(kind) => self
                .options
                .get(&kind)
                .cloned()
                .expect("validated fixed transition should have selected option"),
            TransitionSelection::Random => self
                .options
                .values()
                .choose(rng)
                .cloned()
                .expect("validated random transition should have options"),
        }
    }

    pub fn validate(&mut self) -> Result<()> {
        ensure!(
            !self.options.is_empty(),
            "transition configuration must include at least one option"
        );
        for option in self.options.values_mut() {
            option.normalize()?;
        }
        match self.selection {
            TransitionSelection::Fixed(kind) => ensure!(
                self.options.contains_key(&kind),
                format!(
                    "transition.type {} must match a key in transition.options",
                    kind
                )
            ),
            TransitionSelection::Random => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TransitionOptions {
    kind: TransitionKind,
    duration_ms: u64,
    mode: TransitionMode,
}

impl TransitionOptions {
    fn default_for(kind: TransitionKind) -> Self {
        Self {
            kind,
            duration_ms: 400,
            mode: match kind {
                TransitionKind::Fade => TransitionMode::Fade(FadeTransition::default()),
                TransitionKind::Wipe => TransitionMode::Wipe(WipeTransition::default()),
                TransitionKind::Push => TransitionMode::Push(PushTransition::default()),
            },
        }
    }

    pub fn kind(&self) -> TransitionKind {
        self.kind
    }

    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms.max(1))
    }

    pub fn mode(&self) -> &TransitionMode {
        &self.mode
    }

    fn normalize(&mut self) -> Result<()> {
        ensure!(
            self.duration_ms > 0,
            format!("transition option {} must set duration-ms > 0", self.kind)
        );
        match &mut self.mode {
            TransitionMode::Fade(_) => {}
            TransitionMode::Wipe(wipe) => {
                if !wipe.softness.is_finite() {
                    return Err(anyhow::anyhow!(
                        "transition option {} has non-finite wipe.softness",
                        self.kind
                    ));
                }
                wipe.softness = wipe.softness.clamp(0.0, 0.5);
                if !wipe.angle_deg.is_finite() || !wipe.angle_jitter_deg.is_finite() {
                    return Err(anyhow::anyhow!(
                        "transition option {} has invalid wipe angle configuration",
                        self.kind
                    ));
                }
                ensure!(
                    wipe.angle_jitter_deg >= 0.0,
                    format!(
                        "transition option {} requires wipe.angle-jitter-deg >= 0",
                        self.kind
                    )
                );
            }
            TransitionMode::Push(push) => {
                if !push.angle_deg.is_finite() || !push.angle_jitter_deg.is_finite() {
                    return Err(anyhow::anyhow!(
                        "transition option {} has invalid push angle configuration",
                        self.kind
                    ));
                }
                ensure!(
                    push.angle_jitter_deg >= 0.0,
                    format!(
                        "transition option {} requires push.angle-jitter-deg >= 0",
                        self.kind
                    )
                );
            }
        }
        Ok(())
    }

    fn with_kind(kind: TransitionKind, builder: TransitionOptionBuilder) -> Result<Self> {
        let duration_ms = builder
            .duration_ms
            .unwrap_or_else(|| TransitionOptions::default_for(kind).duration_ms);
        let mode = match kind {
            TransitionKind::Fade => TransitionMode::Fade(FadeTransition {
                through_black: builder.fade_through_black.unwrap_or(false),
            }),
            TransitionKind::Wipe => TransitionMode::Wipe(WipeTransition {
                angle_deg: builder.wipe_angle_deg.unwrap_or(0.0),
                angle_jitter_deg: builder.wipe_angle_jitter_deg.unwrap_or(0.0),
                reverse: builder.wipe_reverse.unwrap_or(false),
                randomize_direction: builder.wipe_randomize_direction.unwrap_or(false),
                softness: builder.wipe_softness.unwrap_or(0.05),
            }),
            TransitionKind::Push => {
                let vertical_axis = builder.push_vertical_axis.unwrap_or(false);
                let angle =
                    builder
                        .push_angle_deg
                        .unwrap_or(if vertical_axis { 90.0 } else { 0.0 });
                TransitionMode::Push(PushTransition {
                    angle_deg: angle,
                    angle_jitter_deg: builder.push_angle_jitter_deg.unwrap_or(0.0),
                    reverse: builder.push_reverse.unwrap_or(false),
                    randomize_direction: builder.push_randomize_direction.unwrap_or(false),
                })
            }
        };
        let mut option = Self {
            kind,
            duration_ms,
            mode,
        };

        match &mut option.mode {
            TransitionMode::Fade(_) => {}
            TransitionMode::Wipe(cfg) => {
                ensure!(
                    cfg.angle_jitter_deg >= 0.0,
                    format!(
                        "transition option {} requires wipe.angle-jitter-deg >= 0",
                        kind
                    )
                );
            }
            TransitionMode::Push(cfg) => {
                ensure!(
                    cfg.angle_jitter_deg >= 0.0,
                    format!(
                        "transition option {} requires push.angle-jitter-deg >= 0",
                        kind
                    )
                );
            }
        }

        Ok(option)
    }
}

#[derive(Debug, Clone)]
pub enum TransitionMode {
    Fade(FadeTransition),
    Wipe(WipeTransition),
    Push(PushTransition),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FadeTransition {
    pub through_black: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct WipeTransition {
    pub angle_deg: f32,
    pub angle_jitter_deg: f32,
    pub reverse: bool,
    pub randomize_direction: bool,
    pub softness: f32,
}

impl Default for WipeTransition {
    fn default() -> Self {
        Self {
            angle_deg: 0.0,
            angle_jitter_deg: 0.0,
            reverse: false,
            randomize_direction: false,
            softness: 0.05,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PushTransition {
    pub angle_deg: f32,
    pub angle_jitter_deg: f32,
    pub reverse: bool,
    pub randomize_direction: bool,
}

impl Default for PushTransition {
    fn default() -> Self {
        Self {
            angle_deg: 0.0,
            angle_jitter_deg: 0.0,
            reverse: false,
            randomize_direction: false,
        }
    }
}

impl<'de> Deserialize<'de> for TransitionConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(TransitionConfigVisitor)
    }
}

struct TransitionConfigVisitor;

impl<'de> Visitor<'de> for TransitionConfigVisitor {
    type Value = TransitionConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a transition configuration map")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut selection: Option<TransitionSelection> = None;
        let mut options: Option<BTreeMap<TransitionKind, TransitionOptions>> = None;
        let mut inline_fields: Vec<(String, YamlValue)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => {
                    if selection.is_some() {
                        return Err(de::Error::duplicate_field("type"));
                    }
                    let raw: String = map.next_value()?;
                    selection = Some(parse_transition_selection(&raw).map_err(de::Error::custom)?);
                }
                "options" => {
                    if options.is_some() {
                        return Err(de::Error::duplicate_field("options"));
                    }
                    options = Some(map.next_value_seed(TransitionOptionsMapSeed)?);
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
            TransitionSelection::Random => {
                if !inline_fields.is_empty() {
                    return Err(de::Error::custom(
                        "transition.type random does not support inline transition fields",
                    ));
                }
            }
            TransitionSelection::Fixed(kind) => {
                if options.is_empty() {
                    let mut builder = TransitionOptionBuilder::default();
                    for (field, value) in std::mem::take(&mut inline_fields) {
                        apply_transition_inline_field::<A::Error>(
                            &mut builder,
                            kind,
                            &field,
                            value,
                        )?;
                    }
                    let option = TransitionOptions::with_kind(kind, builder)
                        .map_err(|err| de::Error::custom(err.to_string()))?;
                    options.insert(kind, option);
                } else {
                    if !inline_fields.is_empty() {
                        return Err(de::Error::custom(
                            "transition configuration cannot mix inline fields with transition.options",
                        ));
                    }
                    if !options.contains_key(&kind) {
                        return Err(de::Error::custom(format!(
                            "transition.type {} must match a key in transition.options",
                            kind
                        )));
                    }
                }
            }
        }

        if options.is_empty() {
            return Err(de::Error::custom(
                "transition.options must include at least one entry",
            ));
        }

        Ok(TransitionConfig { selection, options })
    }
}

fn parse_transition_selection(raw: &str) -> Result<TransitionSelection> {
    if raw == "random" {
        return Ok(TransitionSelection::Random);
    }
    for kind in TransitionKind::ALL {
        if raw == kind.as_str() {
            return Ok(TransitionSelection::Fixed(*kind));
        }
    }
    Err(anyhow::anyhow!(
        "unknown transition type '{}', expected one of: random, {}",
        raw,
        TransitionKind::NAMES.join(", ")
    ))
}

struct TransitionOptionsMapSeed;

impl<'de> DeserializeSeed<'de> for TransitionOptionsMapSeed {
    type Value = BTreeMap<TransitionKind, TransitionOptions>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(TransitionOptionsMapVisitor)
    }
}

struct TransitionOptionsMapVisitor;

impl<'de> Visitor<'de> for TransitionOptionsMapVisitor {
    type Value = BTreeMap<TransitionKind, TransitionOptions>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map of transition options keyed by transition type")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut options = BTreeMap::new();
        while let Some(kind) = map.next_key::<TransitionKind>()? {
            if options.contains_key(&kind) {
                return Err(de::Error::custom(format!(
                    "duplicate transition option for type {}",
                    kind
                )));
            }
            let option = map.next_value_seed(TransitionOptionSeed { kind })?;
            if option.kind() != kind {
                return Err(de::Error::custom(format!(
                    "transition option for key {} does not match its configuration",
                    kind
                )));
            }
            options.insert(kind, option);
        }
        Ok(options)
    }
}

struct TransitionOptionSeed {
    kind: TransitionKind,
}

impl<'de> DeserializeSeed<'de> for TransitionOptionSeed {
    type Value = TransitionOptions;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(TransitionOptionVisitor { kind: self.kind })
    }
}

struct TransitionOptionVisitor {
    kind: TransitionKind,
}

impl<'de> Visitor<'de> for TransitionOptionVisitor {
    type Value = TransitionOptions;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a transition option configuration map")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut builder = TransitionOptionBuilder::default();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value::<YamlValue>()?;
            apply_transition_inline_field::<A::Error>(&mut builder, self.kind, &key, value)?;
        }
        TransitionOptions::with_kind(self.kind, builder)
            .map_err(|err| de::Error::custom(err.to_string()))
    }
}

#[derive(Default)]
struct TransitionOptionBuilder {
    duration_ms: Option<u64>,
    fade_through_black: Option<bool>,
    wipe_angle_deg: Option<f32>,
    wipe_angle_jitter_deg: Option<f32>,
    wipe_reverse: Option<bool>,
    wipe_randomize_direction: Option<bool>,
    wipe_softness: Option<f32>,
    push_angle_deg: Option<f32>,
    push_angle_jitter_deg: Option<f32>,
    push_reverse: Option<bool>,
    push_randomize_direction: Option<bool>,
    push_vertical_axis: Option<bool>,
}

fn apply_transition_inline_field<E: de::Error>(
    builder: &mut TransitionOptionBuilder,
    kind: TransitionKind,
    field: &str,
    value: YamlValue,
) -> Result<(), E> {
    match field {
        "duration-ms" => {
            builder.duration_ms = Some(inline_value_to::<u64, E>(value)?);
        }
        "through-black" if matches!(kind, TransitionKind::Fade) => {
            builder.fade_through_black = Some(inline_value_to::<bool, E>(value)?);
        }
        "angle-deg" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let angle = inline_value_to::<f32, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_angle_deg = Some(angle),
                TransitionKind::Push => builder.push_angle_deg = Some(angle),
                _ => {}
            }
        }
        "angle-jitter-deg" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let jitter = inline_value_to::<f32, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_angle_jitter_deg = Some(jitter),
                TransitionKind::Push => builder.push_angle_jitter_deg = Some(jitter),
                _ => {}
            }
        }
        "reverse" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let reverse = inline_value_to::<bool, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_reverse = Some(reverse),
                TransitionKind::Push => builder.push_reverse = Some(reverse),
                _ => {}
            }
        }
        "randomize-direction" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let randomize = inline_value_to::<bool, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_randomize_direction = Some(randomize),
                TransitionKind::Push => builder.push_randomize_direction = Some(randomize),
                _ => {}
            }
        }
        "vertical-axis" if matches!(kind, TransitionKind::Push) => {
            builder.push_vertical_axis = Some(inline_value_to::<bool, E>(value)?);
        }
        "softness" if matches!(kind, TransitionKind::Wipe) => {
            builder.wipe_softness = Some(inline_value_to::<f32, E>(value)?);
        }
        _ => {
            return Err(de::Error::unknown_field(
                field,
                &[
                    "duration-ms",
                    "through-black",
                    "angle-deg",
                    "angle-jitter-deg",
                    "reverse",
                    "randomize-direction",
                    "softness",
                    "vertical-axis",
                ],
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Configuration {
    /// Root directory to scan recursively for images.
    pub photo_library_path: PathBuf,
    /// GPU render oversample factor relative to screen size (1.0 = native).
    pub oversample: f32,
    /// Transition behavior between successive photos.
    pub transition: TransitionConfig,
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
        ensure!(self.dwell_ms > 0, "dwell-ms must be greater than zero");
        self.transition
            .validate()
            .context("invalid transition configuration")?;
        self.matting
            .prepare_runtime()
            .context("invalid matting configuration")?;
        self.playlist.validate()?;
        Ok(self)
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            photo_library_path: PathBuf::new(),
            oversample: 1.0,
            transition: TransitionConfig::default(),
            dwell_ms: 2000,
            viewer_preload_count: 3,
            loader_max_concurrent_decodes: 4,
            startup_shuffle_seed: None,
            matting: MattingConfig::default(),
            playlist: PlaylistOptions::default(),
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
