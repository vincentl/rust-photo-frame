use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MattingSelection {
    Fixed(MattingKind),
    Random(Vec<MattingKind>),
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
        let mut requested_types: Option<Vec<MattingKind>> = None;
        let mut legacy_type: Option<LegacyMattingType> = None;
        let mut options: Option<BTreeMap<MattingKind, MattingOptions>> = None;
        let mut inline_fields: Vec<(String, YamlValue)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => {
                    if legacy_type.is_some() {
                        return Err(de::Error::duplicate_field("type"));
                    }
                    if requested_types.is_some() {
                        return Err(de::Error::custom(
                            "matting configuration cannot mix type and types",
                        ));
                    }
                    let raw: String = map.next_value()?;
                    legacy_type = Some(parse_legacy_matting_type(&raw).map_err(de::Error::custom)?);
                }
                "types" => {
                    if requested_types.is_some() {
                        return Err(de::Error::duplicate_field("types"));
                    }
                    if legacy_type.is_some() {
                        return Err(de::Error::custom(
                            "matting configuration cannot mix type and types",
                        ));
                    }
                    let raw: Vec<String> = map.next_value()?;
                    requested_types = Some(
                        parse_matting_types(raw)
                            .map_err(|err| de::Error::custom(err.to_string()))?,
                    );
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

        let types = if let Some(types) = requested_types {
            types
        } else if let Some(selection) = legacy_type {
            match selection {
                LegacyMattingType::Fixed(kind) => vec![kind],
                LegacyMattingType::Random => options.keys().copied().collect(),
            }
        } else {
            return Err(de::Error::missing_field("types"));
        };

        if types.is_empty() {
            if matches!(legacy_type, Some(LegacyMattingType::Random)) {
                return Err(de::Error::custom(
                    "matting.type random requires matting.options to specify at least one entry",
                ));
            }
            return Err(de::Error::custom(
                "matting.types must include at least one entry",
            ));
        }

        let selection = if types.len() == 1 {
            MattingSelection::Fixed(types[0])
        } else {
            MattingSelection::Random(types.clone())
        };

        match &selection {
            MattingSelection::Random(kinds) => {
                if !inline_fields.is_empty() {
                    return Err(de::Error::custom(
                        "matting.types with multiple entries do not support inline matting fields",
                    ));
                }
                for kind in kinds {
                    if !options.contains_key(kind) {
                        return Err(de::Error::custom(format!(
                            "matting.types entry {} must match a key in matting.options",
                            kind
                        )));
                    }
                }
            }
            MattingSelection::Fixed(kind) => {
                let kind = *kind;
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
                            "matting.types entry {} must match a key in matting.options",
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

fn parse_matting_types(raw: Vec<String>) -> Result<Vec<MattingKind>> {
    if raw.is_empty() {
        return Err(anyhow::anyhow!(
            "matting.types must include at least one entry",
        ));
    }

    let mut kinds = Vec::new();
    for entry in raw {
        if entry == "random" {
            return Err(anyhow::anyhow!("matting.types may not include 'random'"));
        }
        let kind = MattingKind::ALL
            .iter()
            .find(|candidate| entry == candidate.as_str())
            .copied()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown matting type '{}', expected one of: {}",
                    entry,
                    MattingKind::NAMES.join(", ")
                )
            })?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }

    Ok(kinds)
}

#[derive(Clone, Copy)]
enum LegacyMattingType {
    Fixed(MattingKind),
    Random,
}

fn parse_legacy_matting_type(raw: &str) -> Result<LegacyMattingType> {
    if raw == "random" {
        return Ok(LegacyMattingType::Random);
    }

    let kind = MattingKind::ALL
        .iter()
        .find(|candidate| raw == candidate.as_str())
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown matting type '{}', expected one of: {}",
                raw,
                MattingKind::NAMES.join(", ")
            )
        })?;

    Ok(LegacyMattingType::Fixed(kind))
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
        self.selection.clone()
    }

    #[allow(dead_code)]
    /// Exposed for integration tests to inspect the configured matting options.
    pub fn options(&self) -> &BTreeMap<MattingKind, MattingOptions> {
        &self.options
    }

    pub fn primary_option(&self) -> Option<&MattingOptions> {
        match &self.selection {
            MattingSelection::Fixed(kind) => self.options.get(kind),
            MattingSelection::Random(kinds) => kinds.iter().find_map(|kind| self.options.get(kind)),
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
        match &self.selection {
            MattingSelection::Fixed(kind) => self
                .options
                .get(kind)
                .cloned()
                .expect("validated fixed matting should have selected option"),
            MattingSelection::Random(kinds) => {
                let kind = kinds
                    .iter()
                    .copied()
                    .choose(rng)
                    .expect("validated random matting should have options");
                self.options
                    .get(&kind)
                    .cloned()
                    .expect("validated random matting should have matching option")
            }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionSelection {
    Fixed(TransitionKind),
    Random(Vec<TransitionKind>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransitionKind {
    Fade,
    Wipe,
    Push,
    EInk,
}

impl TransitionKind {
    const ALL: &'static [Self] = &[Self::Fade, Self::Wipe, Self::Push, Self::EInk];
    const NAMES: &'static [&'static str] = &["fade", "wipe", "push", "e-ink"];

    fn as_str(&self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::Wipe => "wipe",
            Self::Push => "push",
            Self::EInk => "e-ink",
        }
    }

    pub const fn as_index(&self) -> u32 {
        match self {
            Self::Fade => 1,
            Self::Wipe => 2,
            Self::Push => 3,
            Self::EInk => 4,
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
        self.selection.clone()
    }

    #[allow(dead_code)]
    pub fn options(&self) -> &BTreeMap<TransitionKind, TransitionOptions> {
        &self.options
    }

    #[allow(dead_code)]
    pub fn primary_option(&self) -> Option<&TransitionOptions> {
        match &self.selection {
            TransitionSelection::Fixed(kind) => self.options.get(kind),
            TransitionSelection::Random(kinds) => {
                kinds.iter().find_map(|kind| self.options.get(kind))
            }
        }
    }

    pub fn choose_option<R: Rng + ?Sized>(&self, rng: &mut R) -> TransitionOptions {
        match &self.selection {
            TransitionSelection::Fixed(kind) => self
                .options
                .get(kind)
                .cloned()
                .expect("validated fixed transition should have selected option"),
            TransitionSelection::Random(kinds) => {
                let kind = kinds
                    .iter()
                    .copied()
                    .choose(rng)
                    .expect("validated random transition should have options");
                self.options
                    .get(&kind)
                    .cloned()
                    .expect("validated random transition should have matching option")
            }
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
        match &self.selection {
            TransitionSelection::Fixed(kind) => ensure!(
                self.options.contains_key(kind),
                format!(
                    "transition.types entry {} must match a key in transition.options",
                    kind
                )
            ),
            TransitionSelection::Random(kinds) => {
                for kind in kinds {
                    ensure!(
                        self.options.contains_key(kind),
                        "transition.types entry {} must match a key in transition.options",
                        kind
                    );
                }
            }
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
        let (duration_ms, mode) = match kind {
            TransitionKind::Fade => (400, TransitionMode::Fade(FadeTransition::default())),
            TransitionKind::Wipe => (400, TransitionMode::Wipe(WipeTransition::default())),
            TransitionKind::Push => (400, TransitionMode::Push(PushTransition::default())),
            TransitionKind::EInk => (1600, TransitionMode::EInk(EInkTransition::default())),
        };
        Self {
            kind,
            duration_ms,
            mode,
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
                wipe.angles.normalize(self.kind)?;
            }
            TransitionMode::Push(push) => {
                push.angles.normalize(self.kind)?;
            }
            TransitionMode::EInk(eink) => {
                if !eink.reveal_portion.is_finite() {
                    return Err(anyhow::anyhow!(
                        "transition option {} has non-finite e-ink.reveal-portion",
                        self.kind
                    ));
                }
                eink.reveal_portion = eink.reveal_portion.clamp(0.05, 0.95);
                if eink.stripe_count == 0 {
                    eink.stripe_count = 1;
                }
                eink.flash_count = eink.flash_count.min(6);
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
                angles: AnglePicker::from_parts(
                    builder.wipe_angle_list_deg,
                    builder.wipe_angle_selection,
                    builder.wipe_angle_jitter_deg,
                ),
                softness: builder.wipe_softness.unwrap_or(0.05),
            }),
            TransitionKind::Push => TransitionMode::Push(PushTransition {
                angles: AnglePicker::from_parts(
                    builder.push_angle_list_deg,
                    builder.push_angle_selection,
                    builder.push_angle_jitter_deg,
                ),
            }),
            TransitionKind::EInk => {
                let defaults = EInkTransition::default();
                TransitionMode::EInk(EInkTransition {
                    flash_count: builder.eink_flash_count.unwrap_or(defaults.flash_count),
                    reveal_portion: builder
                        .eink_reveal_portion
                        .unwrap_or(defaults.reveal_portion),
                    stripe_count: builder.eink_stripe_count.unwrap_or(defaults.stripe_count),
                    flash_color: builder.eink_flash_color.unwrap_or(defaults.flash_color),
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
                cfg.angles.normalize(kind)?;
            }
            TransitionMode::Push(cfg) => {
                cfg.angles.normalize(kind)?;
            }
            TransitionMode::EInk(eink) => {
                if !eink.reveal_portion.is_finite() {
                    return Err(anyhow::anyhow!(
                        "transition option {} has non-finite e-ink.reveal-portion",
                        kind
                    ));
                }
                eink.reveal_portion = eink.reveal_portion.clamp(0.05, 0.95);
                if eink.stripe_count == 0 {
                    eink.stripe_count = 1;
                }
                eink.flash_count = eink.flash_count.min(6);
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
    EInk(EInkTransition),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AngleSelection {
    Random,
    RoundRobin,
}

#[derive(Debug, Clone)]
struct AnglePickerRuntime {
    next_index: Arc<AtomicUsize>,
}

impl Default for AnglePickerRuntime {
    fn default() -> Self {
        Self {
            next_index: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AnglePicker {
    pub angles_deg: Vec<f32>,
    pub selection: AngleSelection,
    pub jitter_deg: f32,
    runtime: AnglePickerRuntime,
}

impl Default for AnglePicker {
    fn default() -> Self {
        Self {
            angles_deg: vec![0.0],
            selection: AngleSelection::Random,
            jitter_deg: 0.0,
            runtime: AnglePickerRuntime::default(),
        }
    }
}

impl AnglePicker {
    fn from_parts(
        angles_deg: Option<Vec<f32>>,
        selection: Option<AngleSelection>,
        jitter_deg: Option<f32>,
    ) -> Self {
        let picker = Self {
            angles_deg: angles_deg.unwrap_or_else(|| vec![0.0]),
            selection: selection.unwrap_or(AngleSelection::Random),
            jitter_deg: jitter_deg.unwrap_or(0.0),
            runtime: AnglePickerRuntime::default(),
        };
        picker
    }

    fn normalize(&mut self, kind: TransitionKind) -> Result<()> {
        ensure!(
            !self.angles_deg.is_empty(),
            format!(
                "transition option {} requires angle-list-degrees to include at least one entry",
                kind
            )
        );
        for angle in &self.angles_deg {
            ensure!(
                angle.is_finite(),
                format!(
                    "transition option {} has non-finite values in angle-list-degrees",
                    kind
                )
            );
        }
        ensure!(
            self.jitter_deg.is_finite(),
            format!(
                "transition option {} has non-finite angle-jitter-degrees",
                kind
            )
        );
        ensure!(
            self.jitter_deg >= 0.0,
            format!(
                "transition option {} requires angle-jitter-degrees >= 0",
                kind
            )
        );
        Ok(())
    }

    pub(crate) fn pick_angle(&self, rng: &mut impl Rng) -> f32 {
        let base_angle = if self.angles_deg.len() == 1 {
            self.angles_deg[0]
        } else {
            match self.selection {
                AngleSelection::Random => {
                    let index = rng.gen_range(0..self.angles_deg.len());
                    self.angles_deg[index]
                }
                AngleSelection::RoundRobin => {
                    let index = self.runtime.next_index.fetch_add(1, Ordering::Relaxed)
                        % self.angles_deg.len();
                    self.angles_deg[index]
                }
            }
        };
        if self.jitter_deg.abs() > f32::EPSILON {
            let jitter = rng.gen_range(-self.jitter_deg..=self.jitter_deg);
            base_angle + jitter
        } else {
            base_angle
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FadeTransition {
    pub through_black: bool,
}

#[derive(Debug, Clone)]
pub struct WipeTransition {
    pub angles: AnglePicker,
    pub softness: f32,
}

impl Default for WipeTransition {
    fn default() -> Self {
        Self {
            angles: AnglePicker::default(),
            softness: 0.05,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PushTransition {
    pub angles: AnglePicker,
}

impl Default for PushTransition {
    fn default() -> Self {
        Self {
            angles: AnglePicker::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EInkTransition {
    pub flash_count: u32,
    pub reveal_portion: f32,
    pub stripe_count: u32,
    pub flash_color: [u8; 3],
}

impl Default for EInkTransition {
    fn default() -> Self {
        Self {
            flash_count: 3,
            reveal_portion: 0.55,
            stripe_count: 24,
            flash_color: [255, 255, 255],
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
        let mut requested_types: Option<Vec<TransitionKind>> = None;
        let mut legacy_type: Option<LegacyTransitionType> = None;
        let mut options: Option<BTreeMap<TransitionKind, TransitionOptions>> = None;
        let mut inline_fields: Vec<(String, YamlValue)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => {
                    if legacy_type.is_some() {
                        return Err(de::Error::duplicate_field("type"));
                    }
                    if requested_types.is_some() {
                        return Err(de::Error::custom(
                            "transition configuration cannot mix type and types",
                        ));
                    }
                    let raw: String = map.next_value()?;
                    legacy_type =
                        Some(parse_legacy_transition_type(&raw).map_err(de::Error::custom)?);
                }
                "types" => {
                    if requested_types.is_some() {
                        return Err(de::Error::duplicate_field("types"));
                    }
                    if legacy_type.is_some() {
                        return Err(de::Error::custom(
                            "transition configuration cannot mix type and types",
                        ));
                    }
                    let raw: Vec<String> = map.next_value()?;
                    requested_types = Some(
                        parse_transition_types(raw)
                            .map_err(|err| de::Error::custom(err.to_string()))?,
                    );
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
        let types = if let Some(types) = requested_types {
            types
        } else if let Some(selection) = legacy_type {
            match selection {
                LegacyTransitionType::Fixed(kind) => vec![kind],
                LegacyTransitionType::Random => options.keys().copied().collect(),
            }
        } else {
            return Err(de::Error::missing_field("types"));
        };

        if types.is_empty() {
            if matches!(legacy_type, Some(LegacyTransitionType::Random)) {
                return Err(de::Error::custom(
                    "transition.type random requires transition.options to specify at least one entry",
                ));
            }
            return Err(de::Error::custom(
                "transition.types must include at least one entry",
            ));
        }

        let selection = if types.len() == 1 {
            TransitionSelection::Fixed(types[0])
        } else {
            TransitionSelection::Random(types.clone())
        };

        match &selection {
            TransitionSelection::Random(kinds) => {
                if !inline_fields.is_empty() {
                    return Err(de::Error::custom(
                        "transition.types with multiple entries do not support inline transition fields",
                    ));
                }
                for kind in kinds {
                    if !options.contains_key(kind) {
                        return Err(de::Error::custom(format!(
                            "transition.types entry {} must match a key in transition.options",
                            kind
                        )));
                    }
                }
            }
            TransitionSelection::Fixed(kind) => {
                let kind = *kind;
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
                            "transition.types entry {} must match a key in transition.options",
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

fn parse_transition_types(raw: Vec<String>) -> Result<Vec<TransitionKind>> {
    if raw.is_empty() {
        return Err(anyhow::anyhow!(
            "transition.types must include at least one entry",
        ));
    }

    let mut kinds = Vec::new();
    for entry in raw {
        if entry == "random" {
            return Err(anyhow::anyhow!("transition.types may not include 'random'"));
        }
        let kind = TransitionKind::ALL
            .iter()
            .find(|candidate| entry == candidate.as_str())
            .copied()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown transition type '{}', expected one of: {}",
                    entry,
                    TransitionKind::NAMES.join(", ")
                )
            })?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }

    Ok(kinds)
}

#[derive(Clone, Copy)]
enum LegacyTransitionType {
    Fixed(TransitionKind),
    Random,
}

fn parse_legacy_transition_type(raw: &str) -> Result<LegacyTransitionType> {
    if raw == "random" {
        return Ok(LegacyTransitionType::Random);
    }

    let kind = TransitionKind::ALL
        .iter()
        .find(|candidate| raw == candidate.as_str())
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown transition type '{}', expected one of: {}",
                raw,
                TransitionKind::NAMES.join(", ")
            )
        })?;

    Ok(LegacyTransitionType::Fixed(kind))
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
    wipe_angle_list_deg: Option<Vec<f32>>,
    wipe_angle_selection: Option<AngleSelection>,
    wipe_angle_jitter_deg: Option<f32>,
    wipe_softness: Option<f32>,
    push_angle_list_deg: Option<Vec<f32>>,
    push_angle_selection: Option<AngleSelection>,
    push_angle_jitter_deg: Option<f32>,
    eink_flash_count: Option<u32>,
    eink_reveal_portion: Option<f32>,
    eink_stripe_count: Option<u32>,
    eink_flash_color: Option<[u8; 3]>,
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
        "angle-list-degrees" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let angles = inline_value_to::<Vec<f32>, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_angle_list_deg = Some(angles),
                TransitionKind::Push => builder.push_angle_list_deg = Some(angles),
                _ => {}
            }
        }
        "angle-selection" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let selection = inline_value_to::<AngleSelection, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_angle_selection = Some(selection),
                TransitionKind::Push => builder.push_angle_selection = Some(selection),
                _ => {}
            }
        }
        "angle-jitter-degrees" if matches!(kind, TransitionKind::Wipe | TransitionKind::Push) => {
            let jitter = inline_value_to::<f32, E>(value)?;
            match kind {
                TransitionKind::Wipe => builder.wipe_angle_jitter_deg = Some(jitter),
                TransitionKind::Push => builder.push_angle_jitter_deg = Some(jitter),
                _ => {}
            }
        }
        "softness" if matches!(kind, TransitionKind::Wipe) => {
            builder.wipe_softness = Some(inline_value_to::<f32, E>(value)?);
        }
        "flash-count" if matches!(kind, TransitionKind::EInk) => {
            builder.eink_flash_count = Some(inline_value_to::<u32, E>(value)?);
        }
        "reveal-portion" if matches!(kind, TransitionKind::EInk) => {
            builder.eink_reveal_portion = Some(inline_value_to::<f32, E>(value)?);
        }
        "stripe-count" if matches!(kind, TransitionKind::EInk) => {
            builder.eink_stripe_count = Some(inline_value_to::<u32, E>(value)?);
        }
        "flash-color" if matches!(kind, TransitionKind::EInk) => {
            builder.eink_flash_color = Some(inline_value_to::<[u8; 3], E>(value)?);
        }
        _ => {
            return Err(de::Error::unknown_field(
                field,
                &[
                    "duration-ms",
                    "through-black",
                    "angle-list-degrees",
                    "angle-selection",
                    "angle-jitter-degrees",
                    "softness",
                    "flash-count",
                    "reveal-portion",
                    "stripe-count",
                    "flash-color",
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
