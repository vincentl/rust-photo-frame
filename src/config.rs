use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, ensure, Context, Result};
use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use rand::seq::IteratorRandom;
use rand::Rng;
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Unexpected,
    Visitor,
};
use serde::Deserialize;
use serde_yaml::Value as YamlValue;

use crate::platform::display_power::{BacklightSysfs, DisplayPowerController, DisplayPowerPlan};
use crate::processing::fixed_image::FixedImageBackground;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct GreetingScreenColorsConfig {
    pub background: Option<String>,
    pub font: Option<String>,
    pub accent: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct GreetingScreenConfig {
    pub message: Option<String>,
    pub font: Option<String>,
    pub stroke_width: Option<f32>,
    pub corner_radius: Option<f32>,
    pub duration_seconds: Option<f32>,
    #[serde(default)]
    pub colors: GreetingScreenColorsConfig,
}

impl GreetingScreenConfig {
    const DEFAULT_STROKE_WIDTH_DIP: f32 = 16.0;
    const DEFAULT_DURATION_SECONDS: f32 = 4.0;

    pub fn effective_stroke_width_dip(&self) -> f32 {
        let width = self
            .stroke_width
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(Self::DEFAULT_STROKE_WIDTH_DIP);
        width.max(0.1)
    }

    pub fn effective_corner_radius_dip(&self) -> f32 {
        let base = self.effective_stroke_width_dip();
        let radius = self
            .corner_radius
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(base * 0.75);
        radius.max(0.0)
    }

    pub fn effective_duration(&self) -> Duration {
        let seconds = self
            .duration_seconds
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(Self::DEFAULT_DURATION_SECONDS)
            .max(0.0);
        Duration::from_secs_f32(seconds)
    }

    pub fn message_or_default(&self) -> std::borrow::Cow<'_, str> {
        match &self.message {
            Some(msg) if !msg.trim().is_empty() => std::borrow::Cow::Borrowed(msg.as_str()),
            _ => std::borrow::Cow::Borrowed("Initializing…"),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(width) = self.stroke_width {
            ensure!(
                width.is_finite() && width > 0.0,
                "greeting-screen.stroke-width must be positive"
            );
        }
        if let Some(radius) = self.corner_radius {
            ensure!(
                radius.is_finite() && radius >= 0.0,
                "greeting-screen.corner-radius must be non-negative"
            );
        }
        if let Some(duration) = self.duration_seconds {
            ensure!(
                duration.is_finite() && duration >= 0.0,
                "greeting-screen.duration-seconds must be non-negative"
            );
        }
        if let Some(font_name) = &self.font {
            ensure!(
                !font_name.trim().is_empty(),
                "greeting-screen.font must not be blank when provided"
            );
        }
        for (field, value) in [
            ("background", &self.colors.background),
            ("font", &self.colors.font),
            ("accent", &self.colors.accent),
        ] {
            if let Some(color) = value {
                ensure!(
                    !color.trim().is_empty(),
                    "greeting-screen.colors.{field} must not be blank when provided"
                );
            }
        }
        Ok(())
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypeSelection {
    Random,
    Sequential,
}

#[derive(Debug, Clone)]
pub struct SequentialState {
    next_index: Arc<AtomicUsize>,
}

impl Default for SequentialState {
    fn default() -> Self {
        Self {
            next_index: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl SequentialState {
    fn next(&self, len: usize) -> usize {
        self.next_index.fetch_add(1, Ordering::Relaxed) % len
    }
}

#[derive(Debug, Clone)]
pub enum MattingSelection {
    Fixed(MattingKind),
    Random(Vec<MattingKind>),
    Sequential {
        kinds: Vec<MattingKind>,
        runtime: SequentialState,
    },
}

impl PartialEq for MattingSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MattingSelection::Fixed(a), MattingSelection::Fixed(b)) => a == b,
            (MattingSelection::Random(a), MattingSelection::Random(b)) => a == b,
            (
                MattingSelection::Sequential { kinds: a, .. },
                MattingSelection::Sequential { kinds: b, .. },
            ) => a == b,
            _ => false,
        }
    }
}

impl Eq for MattingSelection {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixedImagePathSelection {
    Sequential,
    Random,
}

impl Default for FixedImagePathSelection {
    fn default() -> Self {
        Self::Sequential
    }
}

#[derive(Debug, Clone)]
pub struct FixedImageRuntime {
    backgrounds: Arc<[Arc<FixedImageBackground>]>,
    selection: FixedImagePathSelection,
    sequential: SequentialState,
}

impl FixedImageRuntime {
    fn new(
        backgrounds: Vec<Arc<FixedImageBackground>>,
        selection: FixedImagePathSelection,
    ) -> Self {
        Self {
            backgrounds: Arc::from(backgrounds),
            selection,
            sequential: SequentialState::default(),
        }
    }

    fn select<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<Arc<FixedImageBackground>> {
        if self.backgrounds.is_empty() {
            return None;
        }
        match self.selection {
            FixedImagePathSelection::Sequential => {
                let index = self.sequential.next(self.backgrounds.len());
                Some(Arc::clone(&self.backgrounds[index]))
            }
            FixedImagePathSelection::Random => self.backgrounds.iter().choose(rng).cloned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorSelection {
    Sequential,
    Random,
}

impl Default for ColorSelection {
    fn default() -> Self {
        Self::Sequential
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudioMatColor {
    Rgb([u8; 3]),
    PhotoAverage,
}

impl StudioMatColor {
    fn resolve(self, fallback: [f32; 3]) -> [f32; 3] {
        match self {
            StudioMatColor::Rgb(rgb) => [
                (rgb[0] as f32) / 255.0,
                (rgb[1] as f32) / 255.0,
                (rgb[2] as f32) / 255.0,
            ],
            StudioMatColor::PhotoAverage => fallback,
        }
    }
}

impl<'de> Deserialize<'de> for StudioMatColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MatColorVisitor;

        impl<'de> Visitor<'de> for MatColorVisitor {
            type Value = StudioMatColor;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an RGB triple or the string 'photo-average'")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "photo-average" => Ok(StudioMatColor::PhotoAverage),
                    other => Err(de::Error::invalid_value(Unexpected::Str(other), &self)),
                }
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut rgb = [0u8; 3];
                for (index, channel) in rgb.iter_mut().enumerate() {
                    *channel = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(index, &self))?;
                }
                if seq.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::invalid_length(4, &self));
                }
                Ok(StudioMatColor::Rgb(rgb))
            }
        }

        deserializer.deserialize_any(MatColorVisitor)
    }
}

#[derive(Debug, Clone)]
struct FixedColorRuntime {
    palette: Arc<[[u8; 3]]>,
    selection: ColorSelection,
    sequential: SequentialState,
}

impl FixedColorRuntime {
    fn new(colors: Vec<[u8; 3]>, selection: ColorSelection) -> Self {
        Self {
            palette: colors.into(),
            selection,
            sequential: SequentialState::default(),
        }
    }

    fn select<R: Rng + ?Sized>(&self, rng: &mut R) -> [u8; 3] {
        if self.palette.is_empty() {
            return [0, 0, 0];
        }
        match self.selection {
            ColorSelection::Sequential => {
                let index = self.sequential.next(self.palette.len());
                self.palette[index]
            }
            ColorSelection::Random => self
                .palette
                .iter()
                .copied()
                .choose(rng)
                .expect("non-empty fixed color palette"),
        }
    }
}

#[derive(Debug, Clone)]
struct StudioRuntime {
    palette: Arc<[StudioMatColor]>,
    selection: ColorSelection,
    sequential: SequentialState,
}

impl StudioRuntime {
    fn new(colors: Vec<StudioMatColor>, selection: ColorSelection) -> Self {
        Self {
            palette: colors.into(),
            selection,
            sequential: SequentialState::default(),
        }
    }

    fn select<R: Rng + ?Sized>(&self, rng: &mut R, fallback: [f32; 3]) -> [f32; 3] {
        if self.palette.is_empty() {
            return fallback;
        }
        let choice = match self.selection {
            ColorSelection::Sequential => {
                let index = self.sequential.next(self.palette.len());
                self.palette[index]
            }
            ColorSelection::Random => self
                .palette
                .iter()
                .copied()
                .choose(rng)
                .expect("non-empty studio color palette"),
        };
        choice.resolve(fallback)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MattingRuntime {
    fixed_color: Option<FixedColorRuntime>,
    studio: Option<StudioRuntime>,
    fixed_image: Option<FixedImageRuntime>,
}

impl MattingRuntime {
    pub fn select_fixed_color<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<[u8; 3]> {
        self.fixed_color.as_ref().map(|runtime| runtime.select(rng))
    }

    pub fn select_studio_color<R: Rng + ?Sized>(
        &self,
        rng: &mut R,
        fallback: [f32; 3],
    ) -> Option<[f32; 3]> {
        self.studio
            .as_ref()
            .map(|runtime| runtime.select(rng, fallback))
    }

    pub fn select_fixed_image<R: Rng + ?Sized>(
        &self,
        rng: &mut R,
    ) -> Option<Arc<FixedImageBackground>> {
        self.fixed_image
            .as_ref()
            .and_then(|runtime| runtime.select(rng))
    }
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
    FixedColor {
        #[serde(default = "MattingMode::default_fixed_color_palette")]
        colors: Vec<[u8; 3]>,
        #[serde(default, rename = "color-selection")]
        color_selection: ColorSelection,
    },
    Blur {
        #[serde(default = "MattingMode::default_blur_sigma")]
        sigma: f32,
        #[serde(
            default = "MattingMode::default_blur_sample_scale",
            rename = "sample-scale"
        )]
        sample_scale: f32,
        #[serde(default)]
        backend: BlurBackend,
    },
    Studio {
        #[serde(default = "MattingMode::default_studio_colors")]
        colors: Vec<StudioMatColor>,
        #[serde(default, rename = "color-selection")]
        color_selection: ColorSelection,
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
        #[serde(
            default,
            rename = "path",
            deserialize_with = "deserialize_fixed_image_paths"
        )]
        paths: Vec<PathBuf>,
        #[serde(default, rename = "path-selection")]
        path_selection: FixedImagePathSelection,
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
        Self::Neon
    }
}

impl Default for FixedImageFit {
    fn default() -> Self {
        Self::Cover
    }
}

fn deserialize_fixed_image_paths<'de, D>(deserializer: D) -> Result<Vec<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    struct PathsVisitor;

    impl<'de> Visitor<'de> for PathsVisitor {
        type Value = Vec<PathBuf>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a path string or a list of paths")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![PathBuf::from(value)])
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut paths = Vec::new();
            while let Some(path) = seq.next_element::<PathBuf>()? {
                paths.push(path);
            }
            Ok(paths)
        }
    }

    deserializer.deserialize_any(PathsVisitor)
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
        if let MattingMode::FixedColor {
            colors,
            color_selection,
        } = &self.style
        {
            ensure!(
                !colors.is_empty(),
                "matting.fixed-color.colors must include at least one entry",
            );
            self.runtime.fixed_color =
                Some(FixedColorRuntime::new(colors.clone(), *color_selection));
        }
        if let MattingMode::Studio {
            colors,
            color_selection,
            ..
        } = &self.style
        {
            ensure!(
                !colors.is_empty(),
                "matting.studio.colors must include at least one entry",
            );
            self.runtime.studio = Some(StudioRuntime::new(colors.clone(), *color_selection));
        }
        if let MattingMode::FixedImage {
            paths,
            path_selection,
            ..
        } = &self.style
        {
            if paths.is_empty() {
                return Ok(());
            }

            let mut backgrounds = Vec::with_capacity(paths.len());
            for path in paths {
                let background = FixedImageBackground::new(path.clone()).with_context(|| {
                    format!(
                        "failed to prepare fixed background image at {}",
                        path.display()
                    )
                })?;
                backgrounds.push(Arc::new(background));
            }
            self.runtime.fixed_image = Some(FixedImageRuntime::new(backgrounds, *path_selection));
        }
        Ok(())
    }

    pub fn fixed_color(&self) -> Option<[u8; 3]> {
        match &self.style {
            MattingMode::FixedColor { colors, .. } => colors.first().copied(),
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
                colors: base
                    .fixed_colors
                    .unwrap_or_else(MattingMode::default_fixed_color_palette),
                color_selection: base.color_selection.unwrap_or_default(),
            },
            MattingKind::Blur => MattingMode::Blur {
                sigma: base.sigma.unwrap_or_else(MattingMode::default_blur_sigma),
                sample_scale: base
                    .sample_scale
                    .unwrap_or_else(MattingMode::default_blur_sample_scale),
                backend: base.blur_backend.unwrap_or_default(),
            },
            MattingKind::Studio => MattingMode::Studio {
                colors: base
                    .studio_colors
                    .unwrap_or_else(MattingMode::default_studio_colors),
                color_selection: base.color_selection.unwrap_or_default(),
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
                paths: base
                    .fixed_image_paths
                    .expect("fixed-image matting must supply a path"),
                path_selection: base.fixed_image_path_selection.unwrap_or_default(),
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
    fixed_colors: Option<Vec<[u8; 3]>>,
    color_selection: Option<ColorSelection>,
    sigma: Option<f32>,
    sample_scale: Option<f32>,
    blur_backend: Option<BlurBackend>,
    bevel_width_px: Option<f32>,
    bevel_color: Option<[u8; 3]>,
    texture_strength: Option<f32>,
    warp_period_px: Option<f32>,
    weft_period_px: Option<f32>,
    studio_colors: Option<Vec<StudioMatColor>>,
    fixed_image_paths: Option<Vec<PathBuf>>,
    fixed_image_path_selection: Option<FixedImagePathSelection>,
    fixed_image_fit: Option<FixedImageFit>,
}

fn inline_value_to<T, E>(value: YamlValue) -> Result<T, E>
where
    T: DeserializeOwned,
    E: de::Error,
{
    serde_yaml::from_value(value).map_err(|err| de::Error::custom(err.to_string()))
}

fn inline_value_to_fixed_image_paths<E>(value: YamlValue) -> Result<Vec<PathBuf>, E>
where
    E: de::Error,
{
    match value {
        YamlValue::String(path) => Ok(vec![PathBuf::from(path)]),
        YamlValue::Sequence(entries) => {
            let mut paths = Vec::with_capacity(entries.len());
            for entry in entries {
                paths.push(inline_value_to::<PathBuf, E>(entry)?);
            }
            Ok(paths)
        }
        other => Err(de::Error::custom(format!(
            "fixed-image.path must be a string or list of paths, got {:?}",
            other
        ))),
    }
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
                "colors" => {
                    if builder.fixed_colors.is_some() {
                        return Err(de::Error::duplicate_field("colors"));
                    }
                    builder.fixed_colors = Some(inline_value_to::<Vec<[u8; 3]>, E>(value)?);
                }
                "color-selection" => {
                    if builder.color_selection.is_some() {
                        return Err(de::Error::duplicate_field("color-selection"));
                    }
                    builder.color_selection = Some(inline_value_to::<ColorSelection, E>(value)?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        other,
                        &[
                            "colors",
                            "color-selection",
                            "minimum-mat-percentage",
                            "max-upscale-factor",
                        ],
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
                "sample-scale" => {
                    if builder.sample_scale.is_some() {
                        return Err(de::Error::duplicate_field("sample-scale"));
                    }
                    builder.sample_scale = Some(inline_value_to::<f32, E>(value)?);
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
                            "sample-scale",
                            "backend",
                            "minimum-mat-percentage",
                            "max-upscale-factor",
                        ],
                    ));
                }
            },
            MattingKind::Studio => match other {
                "colors" => {
                    if builder.studio_colors.is_some() {
                        return Err(de::Error::duplicate_field("colors"));
                    }
                    builder.studio_colors = Some(inline_value_to::<Vec<StudioMatColor>, E>(value)?);
                }
                "color-selection" => {
                    if builder.color_selection.is_some() {
                        return Err(de::Error::duplicate_field("color-selection"));
                    }
                    builder.color_selection = Some(inline_value_to::<ColorSelection, E>(value)?);
                }
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
                            "colors",
                            "color-selection",
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
                    if builder.fixed_image_paths.is_some() {
                        return Err(de::Error::duplicate_field("path"));
                    }
                    builder.fixed_image_paths =
                        Some(inline_value_to_fixed_image_paths::<E>(value)?);
                }
                "path-selection" => {
                    if builder.fixed_image_path_selection.is_some() {
                        return Err(de::Error::duplicate_field("path-selection"));
                    }
                    builder.fixed_image_path_selection =
                        Some(inline_value_to::<FixedImagePathSelection, E>(value)?);
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
                            "path-selection",
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
        let mut type_selection: Option<TypeSelection> = None;
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
                "type-selection" => {
                    if type_selection.is_some() {
                        return Err(de::Error::duplicate_field("type-selection"));
                    }
                    if legacy_type.is_some() {
                        return Err(de::Error::custom(
                            "matting configuration cannot mix type-selection with legacy type",
                        ));
                    }
                    type_selection = Some(map.next_value()?);
                }
                _ => {
                    let value = map.next_value::<YamlValue>()?;
                    inline_fields.push((key, value));
                }
            }
        }

        if type_selection.is_some() && requested_types.is_none() {
            return Err(de::Error::custom(
                "matting.type-selection requires matting.types",
            ));
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
            match type_selection.unwrap_or(TypeSelection::Random) {
                TypeSelection::Random => MattingSelection::Random(types.clone()),
                TypeSelection::Sequential => MattingSelection::Sequential {
                    kinds: types.clone(),
                    runtime: SequentialState::default(),
                },
            }
        };

        match &selection {
            MattingSelection::Random(kinds) | MattingSelection::Sequential { kinds, .. } => {
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
                    if matches!(kind, MattingKind::FixedImage)
                        && builder.fixed_image_paths.is_none()
                    {
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
                        "colors" => {
                            if builder.fixed_colors.is_some() {
                                return Err(de::Error::duplicate_field("colors"));
                            }
                            builder.fixed_colors = Some(map.next_value()?);
                        }
                        "color-selection" => {
                            if builder.color_selection.is_some() {
                                return Err(de::Error::duplicate_field("color-selection"));
                            }
                            builder.color_selection = Some(map.next_value()?);
                        }
                        _ => {
                            return Err(de::Error::unknown_field(
                                other,
                                &[
                                    "colors",
                                    "color-selection",
                                    "minimum-mat-percentage",
                                    "max-upscale-factor",
                                ],
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
                        "sample-scale" => {
                            if builder.sample_scale.is_some() {
                                return Err(de::Error::duplicate_field("sample-scale"));
                            }
                            builder.sample_scale = Some(map.next_value()?);
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
                                    "sample-scale",
                                    "backend",
                                    "minimum-mat-percentage",
                                    "max-upscale-factor",
                                ],
                            ))
                        }
                    },
                    MattingKind::Studio => match other {
                        "colors" => {
                            if builder.studio_colors.is_some() {
                                return Err(de::Error::duplicate_field("colors"));
                            }
                            builder.studio_colors = Some(map.next_value()?);
                        }
                        "color-selection" => {
                            if builder.color_selection.is_some() {
                                return Err(de::Error::duplicate_field("color-selection"));
                            }
                            builder.color_selection = Some(map.next_value()?);
                        }
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
                                    "colors",
                                    "color-selection",
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
                            if builder.fixed_image_paths.is_some() {
                                return Err(de::Error::duplicate_field("path"));
                            }
                            let value: YamlValue = map.next_value()?;
                            builder.fixed_image_paths =
                                Some(inline_value_to_fixed_image_paths::<A::Error>(value)?);
                        }
                        "path-selection" => {
                            if builder.fixed_image_path_selection.is_some() {
                                return Err(de::Error::duplicate_field("path-selection"));
                            }
                            builder.fixed_image_path_selection = Some(map.next_value()?);
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
                                    "path-selection",
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

        if matches!(self.kind, MattingKind::FixedImage) && builder.fixed_image_paths.is_none() {
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
            MattingSelection::Sequential { kinds, .. } => {
                kinds.first().and_then(|kind| self.options.get(kind))
            }
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
            MattingSelection::Sequential { kinds, runtime } => {
                let index = runtime.next(kinds.len());
                let kind = kinds[index];
                self.options
                    .get(&kind)
                    .cloned()
                    .expect("validated sequential matting should have matching option")
            }
        }
    }
}

impl Default for MattingMode {
    fn default() -> Self {
        Self::FixedColor {
            colors: Self::default_fixed_color_palette(),
            color_selection: ColorSelection::default(),
        }
    }
}

impl MattingMode {
    const fn default_color() -> [u8; 3] {
        [0, 0, 0]
    }

    fn default_fixed_color_palette() -> Vec<[u8; 3]> {
        vec![Self::default_color()]
    }

    fn default_studio_colors() -> Vec<StudioMatColor> {
        vec![StudioMatColor::PhotoAverage]
    }

    const fn default_blur_sigma() -> f32 {
        32.0
    }

    pub const fn default_blur_sample_scale() -> f32 {
        0.125
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PhotoEffectKind {
    PrintSimulation,
}

impl PhotoEffectKind {
    const ALL: &'static [Self] = &[Self::PrintSimulation];
    const NAMES: &'static [&'static str] = &["print-simulation"];

    fn as_str(&self) -> &'static str {
        match self {
            Self::PrintSimulation => "print-simulation",
        }
    }
}

impl fmt::Display for PhotoEffectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PhotoEffectKind {
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PrintSimulationOptions {
    #[serde(
        default = "PrintSimulationOptions::default_light_angle_degrees",
        rename = "light-angle-degrees"
    )]
    pub light_angle_degrees: f32,
    #[serde(
        default = "PrintSimulationOptions::default_relief_strength",
        rename = "relief-strength"
    )]
    pub relief_strength: f32,
    #[serde(
        default = "PrintSimulationOptions::default_ink_spread",
        rename = "ink-spread"
    )]
    pub ink_spread: f32,
    #[serde(
        default = "PrintSimulationOptions::default_sheen_strength",
        rename = "sheen-strength"
    )]
    pub sheen_strength: f32,
    #[serde(
        default = "PrintSimulationOptions::default_paper_color",
        rename = "paper-color"
    )]
    pub paper_color: [u8; 3],
    #[serde(default)]
    pub debug: bool,
}

impl PrintSimulationOptions {
    const fn default_light_angle_degrees() -> f32 {
        135.0
    }

    const fn default_relief_strength() -> f32 {
        0.35
    }

    const fn default_ink_spread() -> f32 {
        0.18
    }

    const fn default_sheen_strength() -> f32 {
        0.22
    }

    const fn default_paper_color() -> [u8; 3] {
        [245, 244, 240]
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.relief_strength.is_finite() && self.relief_strength >= 0.0,
            "photo-effect.print-simulation.relief-strength must be non-negative"
        );
        ensure!(
            self.ink_spread.is_finite() && self.ink_spread >= 0.0,
            "photo-effect.print-simulation.ink-spread must be non-negative"
        );
        ensure!(
            self.sheen_strength.is_finite() && self.sheen_strength >= 0.0,
            "photo-effect.print-simulation.sheen-strength must be non-negative"
        );
        ensure!(
            self.light_angle_degrees.is_finite(),
            "photo-effect.print-simulation.light-angle-degrees must be a finite value"
        );
        Ok(())
    }
}

impl Default for PrintSimulationOptions {
    fn default() -> Self {
        Self {
            light_angle_degrees: Self::default_light_angle_degrees(),
            relief_strength: Self::default_relief_strength(),
            ink_spread: Self::default_ink_spread(),
            sheen_strength: Self::default_sheen_strength(),
            paper_color: Self::default_paper_color(),
            debug: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PhotoEffectOptions {
    PrintSimulation(PrintSimulationOptions),
}

impl PhotoEffectOptions {
    pub fn kind(&self) -> PhotoEffectKind {
        match self {
            Self::PrintSimulation(_) => PhotoEffectKind::PrintSimulation,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            PhotoEffectOptions::PrintSimulation(options) => options
                .validate()
                .context("invalid print-simulation options"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PhotoEffectSelection {
    Disabled,
    Fixed(PhotoEffectKind),
    Random(Vec<PhotoEffectKind>),
    Sequential {
        kinds: Vec<PhotoEffectKind>,
        runtime: SequentialState,
    },
}

impl PartialEq for PhotoEffectSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PhotoEffectSelection::Disabled, PhotoEffectSelection::Disabled) => true,
            (PhotoEffectSelection::Fixed(a), PhotoEffectSelection::Fixed(b)) => a == b,
            (PhotoEffectSelection::Random(a), PhotoEffectSelection::Random(b)) => a == b,
            (
                PhotoEffectSelection::Sequential { kinds: a, .. },
                PhotoEffectSelection::Sequential { kinds: b, .. },
            ) => a == b,
            _ => false,
        }
    }
}

impl Eq for PhotoEffectSelection {}

#[derive(Debug, Clone)]
pub struct PhotoEffectConfig {
    selection: PhotoEffectSelection,
    options: BTreeMap<PhotoEffectKind, PhotoEffectOptions>,
}

impl Default for PhotoEffectConfig {
    fn default() -> Self {
        Self {
            selection: PhotoEffectSelection::Disabled,
            options: BTreeMap::new(),
        }
    }
}

impl PhotoEffectConfig {
    pub fn is_enabled(&self) -> bool {
        !matches!(self.selection, PhotoEffectSelection::Disabled)
    }

    pub fn choose_option<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<PhotoEffectOptions> {
        match &self.selection {
            PhotoEffectSelection::Disabled => None,
            PhotoEffectSelection::Fixed(kind) => self.options.get(kind).cloned(),
            PhotoEffectSelection::Random(kinds) => kinds
                .iter()
                .copied()
                .choose(rng)
                .and_then(|kind| self.options.get(&kind).cloned()),
            PhotoEffectSelection::Sequential { kinds, runtime } => {
                if kinds.is_empty() {
                    None
                } else {
                    let index = runtime.next(kinds.len());
                    let kind = kinds[index];
                    self.options.get(&kind).cloned()
                }
            }
        }
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        match &self.selection {
            PhotoEffectSelection::Disabled => {}
            PhotoEffectSelection::Fixed(kind) => {
                ensure!(
                    self.options.contains_key(kind),
                    "photo-effect.types entry {kind} must match a key in photo-effect.options"
                );
            }
            PhotoEffectSelection::Random(kinds)
            | PhotoEffectSelection::Sequential { kinds, .. } => {
                ensure!(
                    !kinds.is_empty(),
                    "photo-effect.types must include at least one entry"
                );
                for kind in kinds {
                    ensure!(
                        self.options.contains_key(kind),
                        "photo-effect.types entry {kind} must match a key in photo-effect.options"
                    );
                }
            }
        }

        for option in self.options.values() {
            option.validate()?;
        }

        Ok(())
    }
}

impl<'de> Deserialize<'de> for PhotoEffectConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(PhotoEffectConfigVisitor)
    }
}

struct PhotoEffectConfigVisitor;

impl<'de> Visitor<'de> for PhotoEffectConfigVisitor {
    type Value = PhotoEffectConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a photo effect configuration map")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut types: Option<Vec<PhotoEffectKind>> = None;
        let mut type_selection: Option<TypeSelection> = None;
        let mut options: Option<BTreeMap<PhotoEffectKind, PhotoEffectOptions>> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "types" => {
                    if types.is_some() {
                        return Err(de::Error::duplicate_field("types"));
                    }
                    let raw: Vec<PhotoEffectKind> = map.next_value()?;
                    let mut unique = Vec::new();
                    for kind in raw {
                        if unique.contains(&kind) {
                            return Err(de::Error::custom(format!(
                                "photo-effect.types cannot repeat the entry {}",
                                kind
                            )));
                        }
                        unique.push(kind);
                    }
                    types = Some(unique);
                }
                "type-selection" => {
                    if type_selection.is_some() {
                        return Err(de::Error::duplicate_field("type-selection"));
                    }
                    type_selection = Some(map.next_value()?);
                }
                "options" => {
                    if options.is_some() {
                        return Err(de::Error::duplicate_field("options"));
                    }
                    options = Some(map.next_value_seed(PhotoEffectOptionsMapSeed)?);
                }
                other => {
                    return Err(de::Error::unknown_field(
                        other,
                        &["types", "type-selection", "options"],
                    ));
                }
            }
        }

        if let Some(selection) = type_selection.as_ref() {
            if types.as_ref().map_or(true, |t| t.is_empty()) {
                return Err(de::Error::custom(format!(
                    "photo-effect.type-selection {:?} requires photo-effect.types to select from",
                    selection
                )));
            }
        }

        let mut options = options.unwrap_or_default();
        let types = types.unwrap_or_default();

        let selection = if types.is_empty() {
            PhotoEffectSelection::Disabled
        } else if types.len() == 1 {
            PhotoEffectSelection::Fixed(types[0])
        } else {
            match type_selection.unwrap_or(TypeSelection::Random) {
                TypeSelection::Random => PhotoEffectSelection::Random(types.clone()),
                TypeSelection::Sequential => PhotoEffectSelection::Sequential {
                    kinds: types.clone(),
                    runtime: SequentialState::default(),
                },
            }
        };

        if !matches!(selection, PhotoEffectSelection::Disabled) {
            match &selection {
                PhotoEffectSelection::Fixed(kind) => {
                    if !options.contains_key(kind) {
                        options.insert(
                            *kind,
                            PhotoEffectOptions::PrintSimulation(PrintSimulationOptions::default()),
                        );
                    }
                }
                PhotoEffectSelection::Random(kinds)
                | PhotoEffectSelection::Sequential { kinds, .. } => {
                    for kind in kinds {
                        if !options.contains_key(kind) {
                            return Err(de::Error::custom(format!(
                                "photo-effect.types entry {} must match a key in photo-effect.options",
                                kind
                            )));
                        }
                    }
                }
                PhotoEffectSelection::Disabled => {}
            }
        }

        Ok(PhotoEffectConfig { selection, options })
    }
}

struct PhotoEffectOptionsMapSeed;

impl<'de> DeserializeSeed<'de> for PhotoEffectOptionsMapSeed {
    type Value = BTreeMap<PhotoEffectKind, PhotoEffectOptions>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(PhotoEffectOptionsMapVisitor)
    }
}

struct PhotoEffectOptionsMapVisitor;

impl<'de> Visitor<'de> for PhotoEffectOptionsMapVisitor {
    type Value = BTreeMap<PhotoEffectKind, PhotoEffectOptions>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map of photo effect options keyed by effect type")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut options = BTreeMap::new();
        while let Some(kind) = map.next_key::<PhotoEffectKind>()? {
            if options.contains_key(&kind) {
                return Err(de::Error::custom(format!(
                    "duplicate photo-effect option for type {}",
                    kind
                )));
            }
            let option = map.next_value_seed(PhotoEffectOptionSeed { kind })?;
            if option.kind() != kind {
                return Err(de::Error::custom(format!(
                    "photo-effect option for key {} does not match its configuration",
                    kind
                )));
            }
            options.insert(kind, option);
        }
        Ok(options)
    }
}

struct PhotoEffectOptionSeed {
    kind: PhotoEffectKind,
}

impl<'de> DeserializeSeed<'de> for PhotoEffectOptionSeed {
    type Value = PhotoEffectOptions;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        match self.kind {
            PhotoEffectKind::PrintSimulation => {
                let options = PrintSimulationOptions::deserialize(deserializer)?;
                Ok(PhotoEffectOptions::PrintSimulation(options))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum TransitionSelection {
    Fixed(TransitionKind),
    Random(Vec<TransitionKind>),
    Sequential {
        kinds: Vec<TransitionKind>,
        runtime: SequentialState,
    },
}

impl PartialEq for TransitionSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TransitionSelection::Fixed(a), TransitionSelection::Fixed(b)) => a == b,
            (TransitionSelection::Random(a), TransitionSelection::Random(b)) => a == b,
            (
                TransitionSelection::Sequential { kinds: a, .. },
                TransitionSelection::Sequential { kinds: b, .. },
            ) => a == b,
            _ => false,
        }
    }
}

impl Eq for TransitionSelection {}

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
            TransitionSelection::Sequential { kinds, .. } => {
                kinds.first().and_then(|kind| self.options.get(kind))
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
            TransitionSelection::Sequential { kinds, runtime } => {
                let index = runtime.next(kinds.len());
                let kind = kinds[index];
                self.options
                    .get(&kind)
                    .cloned()
                    .expect("validated sequential transition should have matching option")
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
            TransitionSelection::Random(kinds) | TransitionSelection::Sequential { kinds, .. } => {
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
    Sequential,
}

#[derive(Debug, Clone)]
struct AngleSequenceState {
    next_index: Arc<AtomicUsize>,
}

impl Default for AngleSequenceState {
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
    runtime: AngleSequenceState,
}

impl Default for AnglePicker {
    fn default() -> Self {
        Self {
            angles_deg: vec![0.0],
            selection: AngleSelection::Random,
            jitter_deg: 0.0,
            runtime: AngleSequenceState::default(),
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
            runtime: AngleSequenceState::default(),
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
                    let index = rng.random_range(0..self.angles_deg.len());
                    self.angles_deg[index]
                }
                AngleSelection::Sequential => {
                    let index = self.runtime.next_index.fetch_add(1, Ordering::Relaxed)
                        % self.angles_deg.len();
                    self.angles_deg[index]
                }
            }
        };
        if self.jitter_deg.abs() > f32::EPSILON {
            let jitter = rng.random_range(-self.jitter_deg..=self.jitter_deg);
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
            flash_count: 0,
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
        let mut type_selection: Option<TypeSelection> = None;
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
                "type-selection" => {
                    if type_selection.is_some() {
                        return Err(de::Error::duplicate_field("type-selection"));
                    }
                    if legacy_type.is_some() {
                        return Err(de::Error::custom(
                            "transition configuration cannot mix type-selection with legacy type",
                        ));
                    }
                    type_selection = Some(map.next_value()?);
                }
                _ => {
                    let value = map.next_value::<YamlValue>()?;
                    inline_fields.push((key, value));
                }
            }
        }

        if type_selection.is_some() && requested_types.is_none() {
            return Err(de::Error::custom(
                "transition.type-selection requires transition.types",
            ));
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
            match type_selection.unwrap_or(TypeSelection::Random) {
                TypeSelection::Random => TransitionSelection::Random(types.clone()),
                TypeSelection::Sequential => TransitionSelection::Sequential {
                    kinds: types.clone(),
                    runtime: SequentialState::default(),
                },
            }
        };

        match &selection {
            TransitionSelection::Random(kinds) | TransitionSelection::Sequential { kinds, .. } => {
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
#[serde(rename_all = "kebab-case")]
pub struct DailyHoursSpec {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeekdayName(Weekday);

impl WeekdayName {
    fn as_str(self) -> &'static str {
        weekday_label(self.0)
    }

    fn weekday(self) -> Weekday {
        self.0
    }
}

impl PartialOrd for WeekdayName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WeekdayName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .num_days_from_monday()
            .cmp(&other.0.num_days_from_monday())
    }
}

impl<'de> Deserialize<'de> for WeekdayName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let normalized = raw.trim().to_ascii_lowercase();
        let day = match normalized.as_str() {
            "mon" | "monday" => Weekday::Mon,
            "tue" | "tues" | "tuesday" => Weekday::Tue,
            "wed" | "weds" | "wednesday" => Weekday::Wed,
            "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
            "fri" | "friday" => Weekday::Fri,
            "sat" | "saturday" => Weekday::Sat,
            "sun" | "sunday" => Weekday::Sun,
            _ => {
                return Err(de::Error::custom(format!("unknown weekday '{raw}'")));
            }
        };
        Ok(Self(day))
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct DisplayPowerConfig {
    #[serde(rename = "backlight-path")]
    pub backlight_path: Option<PathBuf>,
    #[serde(rename = "sleep-value")]
    pub sleep_value: Option<String>,
    #[serde(rename = "wake-value")]
    pub wake_value: Option<String>,
    #[serde(rename = "sleep-command")]
    pub sleep_command: Option<String>,
    #[serde(rename = "wake-command")]
    pub wake_command: Option<String>,
}

impl DisplayPowerConfig {
    fn prepare_controller(&self) -> Result<DisplayPowerController> {
        const DEFAULT_SLEEP_COMMAND: &str =
            "wlr-randr --output @OUTPUT@ --off || vcgencmd display_power 0";
        const DEFAULT_WAKE_COMMAND: &str =
            "wlr-randr --output @OUTPUT@ --on  || vcgencmd display_power 1";

        let mut plan = DisplayPowerPlan::default();

        if let Some(path) = &self.backlight_path {
            let sleep_value = self.sleep_value.as_ref().ok_or_else(|| {
                anyhow!(
                    "sleep-mode.display-power.sleep-value is required when backlight-path is set"
                )
            })?;
            ensure!(
                !sleep_value.trim().is_empty(),
                "sleep-mode.display-power.sleep-value must not be blank"
            );

            let wake_value = self.wake_value.as_ref().ok_or_else(|| {
                anyhow!(
                    "sleep-mode.display-power.wake-value is required when backlight-path is set"
                )
            })?;
            ensure!(
                !wake_value.trim().is_empty(),
                "sleep-mode.display-power.wake-value must not be blank"
            );

            plan.sysfs = Some(BacklightSysfs {
                path: path.clone(),
                sleep_value: sleep_value.clone(),
                wake_value: wake_value.clone(),
            });
        }

        plan.sleep_command = self.sleep_command.clone().or_else(|| {
            if plan.sysfs.is_none() {
                Some(DEFAULT_SLEEP_COMMAND.to_string())
            } else {
                None
            }
        });
        plan.wake_command = self.wake_command.clone().or_else(|| {
            if plan.sysfs.is_none() {
                Some(DEFAULT_WAKE_COMMAND.to_string())
            } else {
                None
            }
        });

        DisplayPowerController::new(plan)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SleepModeConfig {
    pub timezone: String,
    #[serde(rename = "on-hours")]
    pub on_hours: DailyHoursSpec,
    #[serde(default, rename = "weekday-override")]
    pub weekday_override: Option<DailyHoursSpec>,
    #[serde(default, rename = "weekend-override")]
    pub weekend_override: Option<DailyHoursSpec>,
    #[serde(default)]
    pub days: BTreeMap<WeekdayName, DailyHoursSpec>,
    #[serde(
        default = "SleepModeConfig::default_dim_brightness",
        rename = "dim-brightness"
    )]
    pub dim_brightness: f32,
    #[serde(default, rename = "display-power")]
    pub display_power: Option<DisplayPowerConfig>,
    #[serde(skip)]
    pub runtime: Option<SleepModeRuntime>,
}

impl SleepModeConfig {
    const fn default_dim_brightness() -> f32 {
        0.05
    }

    pub fn runtime(&self) -> Option<&SleepModeRuntime> {
        self.runtime.as_ref()
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        let base_tz: Tz = self
            .timezone
            .parse()
            .map_err(|_| anyhow!("invalid timezone '{}'", self.timezone))?;

        ensure!(
            (0.0..=1.0).contains(&self.dim_brightness),
            "sleep-mode.dim-brightness must be between 0.0 and 1.0"
        );

        let default = SleepScheduleEntry::new(
            parse_window(&self.on_hours, base_tz, "sleep-mode.on-hours")?,
            "sleep-mode.on-hours",
            ScheduleSource::Default,
        );

        let weekday = match &self.weekday_override {
            Some(spec) => Some(SleepScheduleEntry::new(
                parse_window(spec, base_tz, "sleep-mode.weekday-override")?,
                "sleep-mode.weekday-override",
                ScheduleSource::WeekdayOverride,
            )),
            None => None,
        };

        let weekend = match &self.weekend_override {
            Some(spec) => Some(SleepScheduleEntry::new(
                parse_window(spec, base_tz, "sleep-mode.weekend-override")?,
                "sleep-mode.weekend-override",
                ScheduleSource::WeekendOverride,
            )),
            None => None,
        };

        let mut days = BTreeMap::new();
        for (name, spec) in &self.days {
            let label = format!("sleep-mode.days.{}", name.as_str());
            let window = parse_window(spec, base_tz, &label)?;
            days.insert(
                *name,
                SleepScheduleEntry::new(window, label, ScheduleSource::DayOverride(name.weekday())),
            );
        }

        let display_power = match &self.display_power {
            Some(config) => Some(config.prepare_controller()?),
            None => None,
        };

        self.runtime = Some(SleepModeRuntime {
            base_timezone: base_tz,
            default,
            weekday,
            weekend,
            days,
            dim_brightness: self.dim_brightness,
            display_power,
        });
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SleepModeRuntime {
    base_timezone: Tz,
    default: SleepScheduleEntry,
    weekday: Option<SleepScheduleEntry>,
    weekend: Option<SleepScheduleEntry>,
    days: BTreeMap<WeekdayName, SleepScheduleEntry>,
    dim_brightness: f32,
    display_power: Option<DisplayPowerController>,
}

impl SleepModeRuntime {
    pub fn base_timezone(&self) -> Tz {
        self.base_timezone
    }

    pub fn is_awake(&self, now: DateTime<Utc>) -> bool {
        let day = now.with_timezone(&self.base_timezone).weekday();
        let entry = self.schedule_entry_for(day);
        entry.window.contains(now)
    }

    pub fn dim_brightness(&self) -> f32 {
        self.dim_brightness
    }

    pub fn display_power(&self) -> Option<&DisplayPowerController> {
        self.display_power.as_ref()
    }

    pub fn schedule_snapshot(&self, now: DateTime<Utc>) -> SleepScheduleSnapshot {
        let local = now.with_timezone(&self.base_timezone);
        let entry = self.schedule_entry_for(local.weekday());
        SleepScheduleSnapshot {
            now_utc: now,
            now_local: local,
            timezone: self.base_timezone,
            awake: entry.window.contains(now),
            active_label: entry.label.clone(),
            active_source: entry.source,
            next_transition: self.next_transition_after(now),
        }
    }

    pub fn next_transition_after(&self, now: DateTime<Utc>) -> Option<ScheduleBoundary> {
        let local = now.with_timezone(&self.base_timezone);
        let date = local.date_naive();
        let mut candidates = Vec::new();
        for offset in -1..=2 {
            if let Some(day) = date.checked_add_signed(chrono::Duration::days(offset.into())) {
                candidates.extend(self.boundaries_for_date(day));
            }
        }
        candidates.sort_by_key(|boundary| boundary.at_utc);
        candidates
            .into_iter()
            .find(|boundary| boundary.at_utc > now)
    }

    pub fn upcoming_transitions(
        &self,
        start: DateTime<Utc>,
        horizon: chrono::Duration,
    ) -> Vec<ScheduleBoundary> {
        let mut boundaries = Vec::new();
        let end = start + horizon;
        let start_local = start.with_timezone(&self.base_timezone);
        let base_date = start_local.date_naive();
        for offset in -1..=14 {
            if let Some(day) = base_date.checked_add_signed(chrono::Duration::days(offset.into())) {
                for boundary in self.boundaries_for_date(day) {
                    if boundary.at_utc >= start && boundary.at_utc <= end {
                        boundaries.push(boundary);
                    }
                }
            }
        }
        boundaries.sort_by_key(|boundary| boundary.at_utc);
        boundaries
    }

    fn schedule_entry_for(&self, day: Weekday) -> &SleepScheduleEntry {
        if let Some(entry) = self.days.get(&WeekdayName(day)) {
            return entry;
        }
        match day {
            Weekday::Sat | Weekday::Sun => self.weekend.as_ref().unwrap_or(&self.default),
            _ => self.weekday.as_ref().unwrap_or(&self.default),
        }
    }

    fn boundaries_for_date(&self, date: chrono::NaiveDate) -> Vec<ScheduleBoundary> {
        let weekday = date.weekday();
        let entry = self.schedule_entry_for(weekday);
        entry
            .window
            .boundaries_for_date(date)
            .into_iter()
            .map(|(local, awake)| {
                let utc = local.with_timezone(&Utc);
                ScheduleBoundary {
                    at_utc: utc,
                    at_local: utc.with_timezone(&self.base_timezone),
                    awake,
                    label: entry.label.clone(),
                    source: entry.source,
                    weekday,
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct SleepScheduleEntry {
    window: SleepWindow,
    label: String,
    source: ScheduleSource,
}

impl SleepScheduleEntry {
    fn new(window: SleepWindow, label: impl Into<String>, source: ScheduleSource) -> Self {
        Self {
            window,
            label: label.into(),
            source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleSource {
    Default,
    WeekdayOverride,
    WeekendOverride,
    DayOverride(Weekday),
}

#[derive(Debug, Clone)]
pub struct SleepScheduleSnapshot {
    pub now_utc: DateTime<Utc>,
    pub now_local: DateTime<Tz>,
    pub timezone: Tz,
    pub awake: bool,
    pub active_label: String,
    pub active_source: ScheduleSource,
    pub next_transition: Option<ScheduleBoundary>,
}

#[derive(Debug, Clone)]
pub struct ScheduleBoundary {
    pub at_utc: DateTime<Utc>,
    pub at_local: DateTime<Tz>,
    pub awake: bool,
    pub label: String,
    pub source: ScheduleSource,
    pub weekday: Weekday,
}

#[derive(Debug, Clone)]
struct SleepWindow {
    timezone: Tz,
    start: NaiveTime,
    end: NaiveTime,
    wraps: bool,
}

impl SleepWindow {
    fn contains(&self, instant: DateTime<Utc>) -> bool {
        let local = instant.with_timezone(&self.timezone);
        let time = local.time();
        if self.wraps {
            time >= self.start || time < self.end
        } else {
            self.start <= time && time < self.end
        }
    }

    fn boundaries_for_date(&self, date: chrono::NaiveDate) -> Vec<(DateTime<Tz>, bool)> {
        let mut points = Vec::new();
        let start = date.and_time(self.start);
        if let Some(dt) = resolve_local_datetime(self.timezone, start) {
            points.push((dt, true));
        }

        let end_date = if self.wraps {
            date.checked_add_signed(chrono::Duration::days(1))
                .unwrap_or(date)
        } else {
            date
        };
        let end = end_date.and_time(self.end);
        if let Some(dt) = resolve_local_datetime(self.timezone, end) {
            points.push((dt, false));
        }
        points
    }
}

fn resolve_local_datetime(tz: Tz, naive: chrono::NaiveDateTime) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt),
        chrono::LocalResult::Ambiguous(a, b) => Some(a.min(b)),
        chrono::LocalResult::None => {
            let mut adjusted = naive;
            for _ in 0..180 {
                adjusted += chrono::Duration::minutes(1);
                match tz.from_local_datetime(&adjusted) {
                    chrono::LocalResult::Single(dt) => return Some(dt),
                    chrono::LocalResult::Ambiguous(a, b) => return Some(a.min(b)),
                    chrono::LocalResult::None => continue,
                }
            }
            None
        }
    }
}

fn parse_window(spec: &DailyHoursSpec, base_tz: Tz, label: &str) -> Result<SleepWindow> {
    let (start, start_tz) =
        parse_time_with_optional_zone(&spec.start, base_tz, &format!("{label}.start"))?;
    let (end, end_tz) = parse_time_with_optional_zone(&spec.end, base_tz, &format!("{label}.end"))?;
    ensure!(
        start_tz == end_tz,
        "{label}: start and end must share the same timezone",
        label = label
    );
    ensure!(
        start != end,
        "{label}: start and end must not be identical",
        label = label
    );
    Ok(SleepWindow {
        timezone: start_tz,
        start,
        end,
        wraps: end <= start,
    })
}

fn parse_time_with_optional_zone(
    raw: &str,
    default_tz: Tz,
    label: &str,
) -> Result<(NaiveTime, Tz)> {
    let trimmed = raw.trim();
    ensure!(
        !trimmed.is_empty(),
        "{label} must not be blank",
        label = label
    );
    let mut parts = trimmed.split_whitespace();
    let time_part = parts
        .next()
        .ok_or_else(|| anyhow!("{label} must include a time component"))?;
    let tz_part = parts.next();
    ensure!(
        parts.next().is_none(),
        "{label} has unexpected extra tokens",
        label = label
    );

    let time = parse_clock_time(time_part)
        .with_context(|| format!("{label} has invalid time '{time_part}'"))?;
    let timezone = match tz_part {
        Some(name) => name
            .parse()
            .map_err(|_| anyhow!("{label} timezone '{name}' is not recognized"))?,
        None => default_tz,
    };
    Ok((time, timezone))
}

fn parse_clock_time(raw: &str) -> Result<NaiveTime> {
    for fmt in ["%H:%M:%S", "%H:%M", "%R"] {
        if let Ok(time) = NaiveTime::parse_from_str(raw, fmt) {
            return Ok(time);
        }
    }
    Err(anyhow!("invalid time '{raw}'"))
}

fn weekday_label(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "monday",
        Weekday::Tue => "tuesday",
        Weekday::Wed => "wednesday",
        Weekday::Thu => "thursday",
        Weekday::Fri => "friday",
        Weekday::Sat => "saturday",
        Weekday::Sun => "sunday",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::Tz;

    #[test]
    fn sleep_mode_schedule_respects_overrides() {
        let yaml = r#"
photo-library-path: /tmp/photos
sleep-mode:
  timezone: America/Los_Angeles
  on-hours:
    start: "07:00"
    end: "21:00"
  weekend-override:
    start: "09:00"
    end: "23:00"
  days:
    monday:
      start: "10:00"
      end: "20:00"
  dim-brightness: 0.1
"#;
        let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
        let cfg = cfg.validated().unwrap();
        let runtime = cfg
            .sleep_mode
            .as_ref()
            .and_then(SleepModeConfig::runtime)
            .cloned()
            .expect("sleep runtime should exist");

        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        let monday_awake = tz
            .with_ymd_and_hms(2024, 7, 1, 11, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(runtime.is_awake(monday_awake));

        let monday_sleep = tz
            .with_ymd_and_hms(2024, 7, 1, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(!runtime.is_awake(monday_sleep));

        let saturday_awake = tz
            .with_ymd_and_hms(2024, 7, 6, 9, 30, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(runtime.is_awake(saturday_awake));

        let saturday_sleep = tz
            .with_ymd_and_hms(2024, 7, 6, 8, 30, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(!runtime.is_awake(saturday_sleep));

        assert!((runtime.dim_brightness() - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn sleep_mode_supports_wrap_past_midnight() {
        let yaml = r#"
photo-library-path: /tmp/photos
sleep-mode:
  timezone: America/New_York
  on-hours:
    start: "22:00"
    end: "08:00"
"#;
        let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
        let cfg = cfg.validated().unwrap();
        let runtime = cfg
            .sleep_mode
            .as_ref()
            .and_then(SleepModeConfig::runtime)
            .cloned()
            .expect("sleep runtime should exist");
        let tz: Tz = "America/New_York".parse().unwrap();

        let late_evening = tz
            .with_ymd_and_hms(2024, 6, 1, 23, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(runtime.is_awake(late_evening));

        let early_morning = tz
            .with_ymd_and_hms(2024, 6, 2, 6, 30, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(runtime.is_awake(early_morning));

        let midday = tz
            .with_ymd_and_hms(2024, 6, 2, 12, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(!runtime.is_awake(midday));

        let next_boundary = runtime.next_transition_after(midday).unwrap();
        assert_eq!(
            next_boundary.at_local.time(),
            chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap()
        );
        assert!(next_boundary.awake);
    }

    #[test]
    fn sleep_mode_dst_transitions_remain_on_wall_clock() {
        let yaml = r#"
photo-library-path: /tmp/photos
sleep-mode:
  timezone: America/New_York
  on-hours:
    start: "08:00"
    end: "22:00"
"#;
        let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
        let cfg = cfg.validated().unwrap();
        let runtime = cfg
            .sleep_mode
            .as_ref()
            .and_then(SleepModeConfig::runtime)
            .cloned()
            .expect("sleep runtime should exist");
        let tz: Tz = "America/New_York".parse().unwrap();

        let before_dst = tz
            .with_ymd_and_hms(2024, 3, 10, 7, 30, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let next = runtime.next_transition_after(before_dst).unwrap();
        assert_eq!(
            next.at_local.time(),
            chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap()
        );

        let before_fall_back = tz
            .with_ymd_and_hms(2024, 11, 3, 21, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let boundary = runtime.next_transition_after(before_fall_back).unwrap();
        assert_eq!(
            boundary.at_local.time(),
            chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap()
        );
    }

    #[test]
    fn sleep_mode_upcoming_transitions_reports_schedule() {
        let yaml = r#"
photo-library-path: /tmp/photos
sleep-mode:
  timezone: America/Los_Angeles
  on-hours:
    start: "08:00"
    end: "22:00"
  weekday-override:
    start: "07:00"
    end: "23:00"
"#;
        let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
        let cfg = cfg.validated().unwrap();
        let runtime = cfg
            .sleep_mode
            .as_ref()
            .and_then(SleepModeConfig::runtime)
            .cloned()
            .expect("sleep runtime should exist");
        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        let start = tz
            .with_ymd_and_hms(2024, 7, 3, 6, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let transitions = runtime.upcoming_transitions(start, chrono::Duration::hours(24));
        assert!(!transitions.is_empty());
        assert!(transitions.iter().any(|b| {
            b.at_local.time() == chrono::NaiveTime::from_hms_opt(7, 0, 0).unwrap() && b.awake
        }));
        assert!(transitions.iter().any(|b| {
            b.at_local.time() == chrono::NaiveTime::from_hms_opt(23, 0, 0).unwrap() && !b.awake
        }));
    }
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
    /// Optional post-processing effects applied after loading and before display.
    pub photo_effect: PhotoEffectConfig,
    /// Matting configuration for displayed photos.
    pub matting: MattingConfig,
    /// Playlist weighting options for how frequently new photos repeat.
    pub playlist: PlaylistOptions,
    /// Greeting screen shown while the first assets are prepared.
    pub greeting_screen: GreetingScreenConfig,
    /// Optional sleep mode schedule controlling when the viewer idles.
    #[serde(default, alias = "sleep")]
    pub sleep_mode: Option<SleepModeConfig>,
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
        self.photo_effect
            .prepare_runtime()
            .context("invalid photo effect configuration")?;
        self.matting
            .prepare_runtime()
            .context("invalid matting configuration")?;
        self.playlist.validate()?;
        self.greeting_screen
            .validate()
            .context("invalid greeting screen configuration")?;
        if let Some(sleep) = &mut self.sleep_mode {
            sleep
                .prepare_runtime()
                .context("invalid sleep-mode configuration")?;
        }
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
            photo_effect: PhotoEffectConfig::default(),
            matting: MattingConfig::default(),
            playlist: PlaylistOptions::default(),
            greeting_screen: GreetingScreenConfig::default(),
            sleep_mode: None,
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
