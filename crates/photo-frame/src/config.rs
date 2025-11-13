use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use rand::Rng;
use rand::seq::IteratorRandom;
use serde::Deserialize;
use serde::de::{self, DeserializeOwned, Deserializer, MapAccess, SeqAccess, Unexpected, Visitor};
use serde_yaml::{Mapping, Value as YamlValue};

use crate::processing::fixed_image::FixedImageBackground;

pub use config_model::{
    AwakeScheduleConfig, GreetingScreenConfig, ScreenMessageConfig, SleepScreenConfig,
};

pub const DEFAULT_CONTROL_SOCKET_PATH: &str = "/run/photo-frame/control.sock";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct GlobalPhotoSettings {
    /// GPU render oversample factor relative to screen size (1.0 = native).
    pub oversample: f32,
    /// Time an image remains fully visible before starting a transition, in ms.
    pub dwell_ms: u64,
    /// Global maximum enlargement applied to the photo when fitting inside the mat.
    pub max_upscale_factor: f32,
}

impl Default for GlobalPhotoSettings {
    fn default() -> Self {
        Self {
            oversample: 1.0,
            dwell_ms: 2000,
            max_upscale_factor: 1.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MattingOptions {
    #[serde(default = "MattingOptions::default_minimum_percentage")] 
    pub minimum_mat_percentage: f32,
    #[serde(default, flatten)]
    pub style: MattingMode,
    #[serde(default, skip_deserializing)]
    pub runtime: MattingRuntime,
}

/// Canonicalized matting configuration entries and their selection metadata.
///
/// Deserialization expands palette-based definitions (e.g. multiple
/// `fixed-color` entries) into the concrete `options` stored here. Each entry
/// in `selection` references these canonical slots by index so duplicates can
/// coexist without clobbering configuration fields.
#[derive(Debug, Clone)]
pub struct MattingConfig {
    selection: MattingSelection,
    options: Vec<MattingOptions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PipelineSelection {
    Fixed,
    Random,
    Sequential,
}

#[derive(Debug)]
struct PipelineEntry<K> {
    kind: K,
    fields: Vec<(String, YamlValue)>,
}

impl<'de, K> Deserialize<'de> for PipelineEntry<K>
where
    K: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(PipelineEntryVisitor::<K> {
            marker: std::marker::PhantomData,
        })
    }
}

struct PipelineEntryVisitor<K> {
    marker: std::marker::PhantomData<K>,
}

impl<'de, K> Visitor<'de> for PipelineEntryVisitor<K>
where
    K: Deserialize<'de>,
{
    type Value = PipelineEntry<K>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a pipeline entry map with a kind tag")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut kind: Option<K> = None;
        let mut fields = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if key == "kind" {
                if kind.is_some() {
                    return Err(de::Error::duplicate_field("kind"));
                }
                kind = Some(map.next_value()?);
            } else {
                let value = map.next_value::<YamlValue>()?;
                fields.push((key, value));
            }
        }

        let kind = kind.ok_or_else(|| de::Error::missing_field("kind"))?;
        Ok(PipelineEntry { kind, fields })
    }
}

fn resolve_pipeline_selection<E>(
    requested: Option<PipelineSelection>,
    len: usize,
    context: &str,
) -> Result<PipelineSelection, E>
where
    E: de::Error,
{
    if len == 0 {
        return Err(de::Error::custom(format!(
            "{} configuration must include at least one active entry",
            context
        )));
    }

    match requested {
        Some(PipelineSelection::Fixed) => {
            if len != 1 {
                return Err(de::Error::custom(format!(
                    "{} selection 'fixed' requires exactly one active entry",
                    context
                )));
            }
            Ok(PipelineSelection::Fixed)
        }
        Some(selection) => Ok(selection),
        None => {
            if len == 1 {
                Ok(PipelineSelection::Fixed)
            } else {
                Ok(PipelineSelection::Random)
            }
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionEntry<K: Copy> {
    pub index: usize,
    pub kind: K,
}

#[derive(Clone, Copy, Debug)]
pub struct SelectedOption<'a, K: Copy, O> {
    pub entry: SelectionEntry<K>,
    pub option: &'a O,
}

enum SelectionEntries<'a, K: Copy> {
    Single(Option<SelectionEntry<K>>),
    Slice(std::iter::Copied<std::slice::Iter<'a, SelectionEntry<K>>>),
}

impl<'a, K: Copy> SelectionEntries<'a, K> {
    fn single(entry: SelectionEntry<K>) -> Self {
        Self::Single(Some(entry))
    }

    fn from_slice(entries: &'a [SelectionEntry<K>]) -> Self {
        Self::Slice(entries.iter().copied())
    }
}

impl<'a, K: Copy> Iterator for SelectionEntries<'a, K> {
    type Item = SelectionEntry<K>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(entry) => entry.take(),
            Self::Slice(iter) => iter.next(),
        }
    }
}

trait SelectedLookup<K: Copy, O> {
    fn lookup(&self, entry: SelectionEntry<K>) -> Option<&O>;
}

impl<K: Copy, O> SelectedLookup<K, O> for [O] {
    fn lookup(&self, entry: SelectionEntry<K>) -> Option<&O> {
        self.get(entry.index)
    }
}

impl<K: Copy + Ord, O> SelectedLookup<K, O> for BTreeMap<K, O> {
    fn lookup(&self, entry: SelectionEntry<K>) -> Option<&O> {
        self.get(&entry.kind)
    }
}

struct SelectedIter<'a, K: Copy, O, S>
where
    S: SelectedLookup<K, O> + ?Sized,
{
    entries: SelectionEntries<'a, K>,
    options: &'a S,
    marker: std::marker::PhantomData<&'a O>,
}

impl<'a, K: Copy, O, S> SelectedIter<'a, K, O, S>
where
    S: SelectedLookup<K, O> + ?Sized,
{
    fn new(entries: SelectionEntries<'a, K>, options: &'a S) -> Self {
        Self {
            entries,
            options,
            marker: std::marker::PhantomData,
        }
    }
}

impl<'a, K: Copy, O, S> Iterator for SelectedIter<'a, K, O, S>
where
    O: 'a,
    S: SelectedLookup<K, O> + ?Sized,
{
    type Item = SelectedOption<'a, K, O>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(entry) = self.entries.next() {
            if let Some(option) = self.options.lookup(entry) {
                return Some(SelectedOption { entry, option });
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub enum MattingSelection {
    Fixed(SelectionEntry<MattingKind>),
    Random(Arc<[SelectionEntry<MattingKind>]>),
    Sequential {
        entries: Arc<[SelectionEntry<MattingKind>]>,
        runtime: SequentialState,
    },
}

pub type SelectedMatting<'a> = SelectedOption<'a, MattingKind, MattingOptions>;

impl PartialEq for MattingSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MattingSelection::Fixed(a), MattingSelection::Fixed(b)) => a == b,
            (MattingSelection::Random(a), MattingSelection::Random(b)) => a.as_ref() == b.as_ref(),
            (
                MattingSelection::Sequential { entries: a, .. },
                MattingSelection::Sequential { entries: b, .. },
            ) => a.as_ref() == b.as_ref(),
            _ => false,
        }
    }
}

impl Eq for MattingSelection {}

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

#[derive(Debug, Clone, Default)]
pub struct MattingRuntime {
    fixed_color: Option<[u8; 3]>,
    studio_color: Option<StudioMatColor>,
    fixed_image: Option<Arc<FixedImageBackground>>,
}

impl MattingRuntime {
    pub fn fixed_color(&self) -> Option<[u8; 3]> {
        self.fixed_color
    }

    pub fn studio_color(&self, fallback: [f32; 3]) -> Option<[f32; 3]> {
        self.studio_color.map(|color| color.resolve(fallback))
    }

    pub fn fixed_image(&self) -> Option<Arc<FixedImageBackground>> {
        self.fixed_image.clone()
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
            style: MattingMode::default(),
            runtime: MattingRuntime::default(),
        }
    }
}

impl MattingOptions {
    const fn default_minimum_percentage() -> f32 {
        0.0
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        self.runtime = MattingRuntime::default();
        if let MattingMode::FixedColor { colors, .. } = &self.style {
            ensure!(
                !colors.is_empty(),
                "matting.fixed-color.colors must include at least one entry",
            );
            self.runtime.fixed_color = colors.first().copied();
        }
        if let MattingMode::Studio { colors, .. } = &self.style {
            ensure!(
                !colors.is_empty(),
                "matting.studio.colors must include at least one entry",
            );
            self.runtime.studio_color = colors.first().copied();
        }
        if let MattingMode::FixedImage { paths, .. } = &self.style {
            if paths.is_empty() {
                return Ok(());
            }

            for path in paths {
                match FixedImageBackground::new(path.clone()) {
                    Ok(background) => {
                        self.runtime.fixed_image = Some(Arc::new(background));
                        break;
                    }
                    Err(err) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %err,
                            "skipping fixed background image that failed to prepare"
                        );
                    }
                }
            }

            if self.runtime.fixed_image.is_none() {
                tracing::warn!(
                    "all configured fixed-image backgrounds failed to load; disabling fixed-image matting"
                );
            }
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

impl MattingOptions {
    fn with_kind(kind: MattingKind, base: MattingOptionBuilder) -> Self {
        let style = match kind {
            MattingKind::FixedColor => MattingMode::FixedColor {
                colors: base
                    .fixed_colors
                    .unwrap_or_else(MattingMode::default_fixed_color_palette),
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
                fit: base.fixed_image_fit.unwrap_or_default(),
            },
        };
        Self {
            minimum_mat_percentage: base
                .minimum_mat_percentage
                .unwrap_or_else(Self::default_minimum_percentage),
            style,
            runtime: MattingRuntime::default(),
        }
    }

    #[allow(dead_code)]
    fn kind(&self) -> MattingKind {
        match &self.style {
            MattingMode::FixedColor { .. } => MattingKind::FixedColor,
            MattingMode::Blur { .. } => MattingKind::Blur,
            MattingMode::Studio { .. } => MattingKind::Studio,
            MattingMode::FixedImage { .. } => MattingKind::FixedImage,
        }
    }
}

#[derive(Default, Clone)]
struct MattingOptionBuilder {
    minimum_mat_percentage: Option<f32>,
    fixed_colors: Option<Vec<[u8; 3]>>,
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
        other => match kind {
            MattingKind::FixedColor => match other {
                "colors" => {
                    if builder.fixed_colors.is_some() {
                        return Err(de::Error::duplicate_field("colors"));
                    }
                    builder.fixed_colors = Some(inline_value_to::<Vec<[u8; 3]>, E>(value)?);
                }
                "color" => {
                    if builder.fixed_colors.is_some() {
                        return Err(de::Error::duplicate_field("color"));
                    }
                    let color = inline_value_to::<[u8; 3], E>(value)?;
                    builder.fixed_colors = Some(vec![color]);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        other,
                        &[
                            "colors",
                            "color",
                            "minimum-mat-percentage",
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
                            "bevel-width-px",
                            "bevel-color",
                            "texture-strength",
                            "warp-period-px",
                            "weft-period-px",
                            "minimum-mat-percentage",
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
                        ],
                    ));
                }
            },
        },
    }
    Ok(())
}

impl MattingOptionBuilder {
    fn into_canonical_options(self, kind: MattingKind) -> Vec<MattingOptions> {
        match kind {
            MattingKind::FixedColor => {
                if let Some(colors) = &self.fixed_colors {
                    if colors.len() > 1 {
                        let mut options = Vec::with_capacity(colors.len());
                        for color in colors.iter().copied() {
                            let mut builder = self.clone();
                            builder.fixed_colors = Some(vec![color]);
                            options.push(MattingOptions::with_kind(kind, builder));
                        }
                        return options;
                    }
                }
            }
            MattingKind::Studio => {
                if let Some(colors) = &self.studio_colors {
                    if colors.len() > 1 {
                        let mut options = Vec::with_capacity(colors.len());
                        for color in colors.iter().copied() {
                            let mut builder = self.clone();
                            builder.studio_colors = Some(vec![color]);
                            options.push(MattingOptions::with_kind(kind, builder));
                        }
                        return options;
                    }
                }
            }
            MattingKind::FixedImage => {
                if let Some(paths) = &self.fixed_image_paths {
                    if paths.len() > 1 {
                        let mut options = Vec::with_capacity(paths.len());
                        for path in paths.iter().cloned() {
                            let mut builder = self.clone();
                            builder.fixed_image_paths = Some(vec![path]);
                            options.push(MattingOptions::with_kind(kind, builder));
                        }
                        return options;
                    }
                }
            }
            MattingKind::Blur => {}
        }

        vec![MattingOptions::with_kind(kind, self)]
    }
}

impl Default for MattingConfig {
    fn default() -> Self {
        let mut options = Vec::new();
        options.push(MattingOptions::default());
        Self {
            selection: MattingSelection::Fixed(SelectionEntry {
                index: 0,
                kind: MattingKind::FixedColor,
            }),
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
        let mut selection: Option<PipelineSelection> = None;
        let mut active: Option<Vec<PipelineEntry<MattingKind>>> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "selection" => {
                    if selection.is_some() {
                        return Err(de::Error::duplicate_field("selection"));
                    }
                    selection = Some(map.next_value()?);
                }
                "active" => {
                    if active.is_some() {
                        return Err(de::Error::duplicate_field("active"));
                    }
                    active = Some(map.next_value()?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        key.as_str(),
                        &["selection", "active"],
                    ));
                }
            }
        }

        let active_entries = active.ok_or_else(|| de::Error::missing_field("active"))?;
        let mut options = Vec::new();
        let mut entries = Vec::new();
        for entry in active_entries.into_iter() {
            let kind = entry.kind;
            let mut builder = MattingOptionBuilder::default();
            for (field, value) in entry.fields {
                apply_inline_field::<A::Error>(&mut builder, kind, &field, value)?;
            }
            if matches!(kind, MattingKind::FixedImage) && builder.fixed_image_paths.is_none() {
                return Err(de::Error::custom(
                    "matting.active entry for fixed-image must include a path",
                ));
            }
            let canonical = builder.into_canonical_options(kind);
            for option in canonical {
                let index = options.len();
                let kind = option.kind();
                options.push(option);
                entries.push(SelectionEntry { index, kind });
            }
        }

        let resolved_selection =
            resolve_pipeline_selection::<A::Error>(selection, options.len(), "matting")?;

        let entries: Arc<[SelectionEntry<MattingKind>]> = entries.into();

        let selection = match resolved_selection {
            PipelineSelection::Fixed => MattingSelection::Fixed(entries[0]),
            PipelineSelection::Random => MattingSelection::Random(entries.clone()),
            PipelineSelection::Sequential => MattingSelection::Sequential {
                entries: entries.clone(),
                runtime: SequentialState::default(),
            },
        };

        Ok(MattingConfig { selection, options })
    }
}

impl MattingConfig {
    /// Exposed for integration tests to introspect the parsed selection strategy.
    pub fn selection(&self) -> &MattingSelection {
        &self.selection
    }

    /// Exposed for integration tests to inspect the configured matting options.
    pub fn options(&self) -> &[MattingOptions] {
        &self.options
    }

    fn selection_entries(&self) -> SelectionEntries<'_, MattingKind> {
        match self.selection() {
            MattingSelection::Fixed(entry) => SelectionEntries::single(*entry),
            MattingSelection::Random(entries) => SelectionEntries::from_slice(entries.as_ref()),
            MattingSelection::Sequential { entries, .. } => {
                SelectionEntries::from_slice(entries.as_ref())
            }
        }
    }

    pub fn iter_selected(&self) -> impl Iterator<Item = SelectedMatting<'_>> {
        SelectedIter::new(self.selection_entries(), self.options.as_slice())
    }

    pub fn primary_selected(&self) -> Option<SelectedMatting<'_>> {
        self.iter_selected().next()
    }

    #[allow(dead_code)]
    pub fn selected_by_index(&self, index: usize) -> Option<SelectedMatting<'_>> {
        self.iter_selected()
            .find(|selected| selected.entry.index == index)
    }

    pub fn select_active<R: Rng + ?Sized>(&self, rng: &mut R) -> SelectedMatting<'_> {
        let entry = match self.selection() {
            MattingSelection::Fixed(entry) => *entry,
            MattingSelection::Random(entries) => *entries
                .iter()
                .choose(rng)
                .expect("validated random matting should have options"),
            MattingSelection::Sequential { entries, runtime } => {
                let index = runtime.next(entries.len());
                entries[index]
            }
        };

        let option = self
            .options
            .get(entry.index)
            .expect("validated matting selection should resolve to an option");
        SelectedOption { entry, option }
    }

    pub fn primary_option(&self) -> Option<&MattingOptions> {
        self.primary_selected().map(|selected| selected.option)
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        ensure!(
            !self.options().is_empty(),
            "matting configuration must include at least one active entry"
        );
        for option in self.options.iter_mut() {
            option
                .prepare_runtime()
                .context("failed to prepare matting resources")?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn choose_option<R: Rng + ?Sized>(&self, rng: &mut R) -> MattingOptions {
        self.select_active(rng).option.clone()
    }
}

impl Default for MattingMode {
    fn default() -> Self {
        Self::FixedColor {
            colors: Self::default_fixed_color_palette(),
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
    Fixed(SelectionEntry<PhotoEffectKind>),
    Random(Arc<[SelectionEntry<PhotoEffectKind>]>),
    Sequential {
        entries: Arc<[SelectionEntry<PhotoEffectKind>]>,
        runtime: SequentialState,
    },
}

impl PartialEq for PhotoEffectSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PhotoEffectSelection::Disabled, PhotoEffectSelection::Disabled) => true,
            (PhotoEffectSelection::Fixed(a), PhotoEffectSelection::Fixed(b)) => a == b,
            (PhotoEffectSelection::Random(a), PhotoEffectSelection::Random(b)) => {
                a.as_ref() == b.as_ref()
            }
            (
                PhotoEffectSelection::Sequential { entries: a, .. },
                PhotoEffectSelection::Sequential { entries: b, .. },
            ) => a.as_ref() == b.as_ref(),
            _ => false,
        }
    }
}

impl Eq for PhotoEffectSelection {}

#[derive(Debug, Clone)]
pub struct PhotoEffectConfig {
    selection: PhotoEffectSelection,
    options: Vec<PhotoEffectOptions>,
}

impl Default for PhotoEffectConfig {
    fn default() -> Self {
        Self {
            selection: PhotoEffectSelection::Disabled,
            options: Vec::new(),
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
            PhotoEffectSelection::Fixed(entry) => self.option_for_entry(*entry).cloned(),
            PhotoEffectSelection::Random(entries) => entries
                .iter()
                .copied()
                .choose(rng)
                .and_then(|entry| self.option_for_entry(entry).cloned()),
            PhotoEffectSelection::Sequential { entries, runtime } => {
                if entries.is_empty() {
                    None
                } else {
                    let index = runtime.next(entries.len());
                    let entry = entries[index];
                    self.option_for_entry(entry).cloned()
                }
            }
        }
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        match &self.selection {
            PhotoEffectSelection::Disabled => return Ok(()),
            PhotoEffectSelection::Fixed(entry) => {
                ensure!(
                    self.option_for_entry(*entry).is_some(),
                    "photo-effect.active entry {} must define options for {}",
                    entry.index,
                    entry.kind,
                );
            }
            PhotoEffectSelection::Random(entries)
            | PhotoEffectSelection::Sequential { entries, .. } => {
                ensure!(
                    !entries.is_empty(),
                    "photo-effect configuration must include at least one active entry",
                );
                for entry in entries.iter() {
                    ensure!(
                        self.option_for_entry(*entry).is_some(),
                        "photo-effect.active entry {} must define options for {}",
                        entry.index,
                        entry.kind,
                    );
                }
            }
        }

        for option in &self.options {
            option.validate()?;
        }

        Ok(())
    }

    fn option_for_entry(
        &self,
        entry: SelectionEntry<PhotoEffectKind>,
    ) -> Option<&PhotoEffectOptions> {
        self.options
            .get(entry.index)
            .filter(|option| option.kind() == entry.kind)
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
        let mut selection: Option<PipelineSelection> = None;
        let mut active: Option<Vec<PipelineEntry<PhotoEffectKind>>> = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "selection" => {
                    if selection.is_some() {
                        return Err(de::Error::duplicate_field("selection"));
                    }
                    selection = Some(map.next_value()?);
                }
                "active" => {
                    if active.is_some() {
                        return Err(de::Error::duplicate_field("active"));
                    }
                    active = Some(map.next_value()?);
                }
                other => {
                    return Err(de::Error::unknown_field(other, &["selection", "active"]));
                }
            }
        }

        let active_entries = active.unwrap_or_default();
        if active_entries.is_empty() {
            return Ok(PhotoEffectConfig {
                selection: PhotoEffectSelection::Disabled,
                options: Vec::new(),
            });
        }

        let mut options = Vec::new();
        let mut entries = Vec::with_capacity(active_entries.len());
        for entry in active_entries.into_iter() {
            let kind = entry.kind;
            let option = build_photo_effect_option::<A::Error>(kind, entry.fields)?;
            let canonical_index = options.len();
            options.push(option);
            entries.push(SelectionEntry {
                index: canonical_index,
                kind,
            });
        }

        let resolved_selection =
            resolve_pipeline_selection::<A::Error>(selection, options.len(), "photo-effect")?;

        let entries: Arc<[SelectionEntry<PhotoEffectKind>]> = entries.into();

        let selection = match resolved_selection {
            PipelineSelection::Fixed => PhotoEffectSelection::Fixed(entries[0]),
            PipelineSelection::Random => PhotoEffectSelection::Random(entries.clone()),
            PipelineSelection::Sequential => PhotoEffectSelection::Sequential {
                entries: entries.clone(),
                runtime: SequentialState::default(),
            },
        };

        Ok(PhotoEffectConfig { selection, options })
    }
}

fn build_photo_effect_option<E>(
    kind: PhotoEffectKind,
    fields: Vec<(String, YamlValue)>,
) -> Result<PhotoEffectOptions, E>
where
    E: de::Error,
{
    let mut mapping = Mapping::new();
    for (field, value) in fields {
        let key = YamlValue::String(field.clone());
        if mapping.insert(key, value).is_some() {
            return Err(de::Error::custom(format!(
                "duplicate photo-effect field {}",
                field
            )));
        }
    }

    let value = YamlValue::Mapping(mapping);
    match kind {
        PhotoEffectKind::PrintSimulation => {
            let options = inline_value_to::<PrintSimulationOptions, E>(value)?;
            Ok(PhotoEffectOptions::PrintSimulation(options))
        }
    }
}

#[derive(Debug, Clone)]
pub enum TransitionSelection {
    Fixed(SelectionEntry<TransitionKind>),
    Random(Arc<[SelectionEntry<TransitionKind>]>),
    Sequential {
        entries: Arc<[SelectionEntry<TransitionKind>]>,
        runtime: SequentialState,
    },
}

pub type SelectedTransition<'a> = SelectedOption<'a, TransitionKind, TransitionOptions>;

impl PartialEq for TransitionSelection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TransitionSelection::Fixed(a), TransitionSelection::Fixed(b)) => a == b,
            (TransitionSelection::Random(a), TransitionSelection::Random(b)) => {
                a.as_ref() == b.as_ref()
            }
            (
                TransitionSelection::Sequential { entries: a, .. },
                TransitionSelection::Sequential { entries: b, .. },
            ) => a.as_ref() == b.as_ref(),
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
    options: Vec<TransitionOptions>,
}

impl Default for TransitionConfig {
    fn default() -> Self {
        let mut options = Vec::new();
        options.push(TransitionOptions::default_for(TransitionKind::Fade));
        Self {
            selection: TransitionSelection::Fixed(SelectionEntry {
                index: 0,
                kind: TransitionKind::Fade,
            }),
            options,
        }
    }
}

impl TransitionConfig {
    pub fn selection(&self) -> &TransitionSelection {
        &self.selection
    }

    #[allow(dead_code)]
    pub fn options(&self) -> &[TransitionOptions] {
        &self.options
    }

    fn selection_entries(&self) -> SelectionEntries<'_, TransitionKind> {
        match self.selection() {
            TransitionSelection::Fixed(entry) => SelectionEntries::single(*entry),
            TransitionSelection::Random(entries) => SelectionEntries::from_slice(entries.as_ref()),
            TransitionSelection::Sequential { entries, .. } => {
                SelectionEntries::from_slice(entries.as_ref())
            }
        }
    }

    pub fn iter_selected(&self) -> impl Iterator<Item = SelectedTransition<'_>> {
        SelectedIter::new(self.selection_entries(), self.options.as_slice())
    }

    pub fn primary_selected(&self) -> Option<SelectedTransition<'_>> {
        self.iter_selected().next()
    }

    #[allow(dead_code)]
    pub fn selected_by_index(&self, index: usize) -> Option<SelectedTransition<'_>> {
        self.iter_selected()
            .find(|selected| selected.entry.index == index)
    }

    pub fn select_active<R: Rng + ?Sized>(&self, rng: &mut R) -> SelectedTransition<'_> {
        let entry = match self.selection() {
            TransitionSelection::Fixed(entry) => *entry,
            TransitionSelection::Random(entries) => *entries
                .iter()
                .choose(rng)
                .expect("validated random transition should have options"),
            TransitionSelection::Sequential { entries, runtime } => {
                let index = runtime.next(entries.len());
                entries[index]
            }
        };

        let option = self
            .options
            .get(entry.index)
            .expect("validated transition selection should resolve to an option");
        SelectedOption { entry, option }
    }

    #[allow(dead_code)]
    pub fn primary_option(&self) -> Option<&TransitionOptions> {
        self.primary_selected().map(|selected| selected.option)
    }

    #[allow(dead_code)]
    pub fn choose_option<R: Rng + ?Sized>(&self, rng: &mut R) -> TransitionOptions {
        self.select_active(rng).option.clone()
    }

    pub fn validate(&mut self) -> Result<()> {
        ensure!(
            !self.options.is_empty(),
            "transition configuration must include at least one active entry"
        );
        for option in self.options.iter_mut() {
            option.normalize()?;
        }
        let validate_entry = |entry: SelectionEntry<TransitionKind>| -> Result<()> {
            let option = self.options.get(entry.index).with_context(|| {
                format!(
                    "transition.active entry {} references index {} which is out of bounds",
                    entry.kind, entry.index
                )
            })?;
            ensure!(
                option.kind == entry.kind,
                "transition.active entry {} at index {} must resolve to a matching option",
                entry.kind,
                entry.index
            );
            Ok(())
        };
        match &self.selection {
            TransitionSelection::Fixed(entry) => validate_entry(*entry)?,
            TransitionSelection::Random(entries)
            | TransitionSelection::Sequential { entries, .. } => {
                for entry in entries.iter() {
                    validate_entry(*entry)?;
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
            TransitionKind::Wipe => {
                if let Some(angles) = builder
                    .wipe_angle_list_deg
                    .as_ref()
                    .filter(|angles| angles.len() > 1)
                {
                    return Err(anyhow::anyhow!(
                        "transition option {} expected canonicalized angle entries, got {} entries",
                        kind,
                        angles.len()
                    ));
                }
                let base = builder
                    .wipe_angle_list_deg
                    .as_ref()
                    .and_then(|angles| angles.first().copied())
                    .unwrap_or(0.0);
                let jitter = builder.wipe_angle_jitter_deg.unwrap_or(0.0);
                TransitionMode::Wipe(WipeTransition {
                    angles: AnglePicker::new(base, jitter),
                    softness: builder.wipe_softness.unwrap_or(0.05),
                })
            }
            TransitionKind::Push => {
                if let Some(angles) = builder
                    .push_angle_list_deg
                    .as_ref()
                    .filter(|angles| angles.len() > 1)
                {
                    return Err(anyhow::anyhow!(
                        "transition option {} expected canonicalized angle entries, got {} entries",
                        kind,
                        angles.len()
                    ));
                }
                let base = builder
                    .push_angle_list_deg
                    .as_ref()
                    .and_then(|angles| angles.first().copied())
                    .unwrap_or(0.0);
                let jitter = builder.push_angle_jitter_deg.unwrap_or(0.0);
                TransitionMode::Push(PushTransition {
                    angles: AnglePicker::new(base, jitter),
                })
            }
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

#[derive(Debug, Clone, Copy)]
pub struct AnglePicker {
    pub base_deg: f32,
    pub jitter_deg: f32,
}

impl Default for AnglePicker {
    fn default() -> Self {
        Self {
            base_deg: 0.0_f32,
            jitter_deg: 0.0,
        }
    }
}

impl AnglePicker {
    fn new(base_deg: f32, jitter_deg: f32) -> Self {
        Self {
            base_deg,
            jitter_deg,
        }
    }

    fn normalize(&mut self, kind: TransitionKind) -> Result<()> {
        ensure!(
            self.base_deg.is_finite(),
            format!(
                "transition option {} has non-finite values in angle-list-degrees",
                kind
            )
        );
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
        let mut selection: Option<PipelineSelection> = None;
        let mut active: Option<Vec<PipelineEntry<TransitionKind>>> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "selection" => {
                    if selection.is_some() {
                        return Err(de::Error::duplicate_field("selection"));
                    }
                    selection = Some(map.next_value()?);
                }
                "active" => {
                    if active.is_some() {
                        return Err(de::Error::duplicate_field("active"));
                    }
                    active = Some(map.next_value()?);
                }
                _ => {
                    return Err(de::Error::unknown_field(
                        key.as_str(),
                        &["selection", "active"],
                    ));
                }
            }
        }

        let active_entries = active.ok_or_else(|| de::Error::missing_field("active"))?;

        let mut options = Vec::new();
        let mut entries = Vec::new();
        for entry in active_entries.into_iter() {
            let kind = entry.kind;
            let mut builder = TransitionOptionBuilder::default();
            for (field, value) in entry.fields {
                apply_transition_inline_field::<A::Error>(&mut builder, kind, &field, value)?;
            }
            let canonical = builder
                .into_canonical_options(kind)
                .map_err(|err| de::Error::custom(err.to_string()))?;
            for option in canonical {
                let index = options.len();
                options.push(option);
                entries.push(SelectionEntry { index, kind });
            }
        }

        let resolved_selection =
            resolve_pipeline_selection::<A::Error>(selection, options.len(), "transition")?;

        let entries: Arc<[SelectionEntry<TransitionKind>]> = entries.into();

        let selection = match resolved_selection {
            PipelineSelection::Fixed => TransitionSelection::Fixed(entries[0]),
            PipelineSelection::Random => TransitionSelection::Random(entries.clone()),
            PipelineSelection::Sequential => TransitionSelection::Sequential {
                entries: entries.clone(),
                runtime: SequentialState::default(),
            },
        };

        Ok(TransitionConfig { selection, options })
    }
}

#[derive(Clone, Default)]
struct TransitionOptionBuilder {
    duration_ms: Option<u64>,
    fade_through_black: Option<bool>,
    wipe_angle_list_deg: Option<Vec<f32>>,
    wipe_angle_jitter_deg: Option<f32>,
    wipe_softness: Option<f32>,
    push_angle_list_deg: Option<Vec<f32>>,
    push_angle_jitter_deg: Option<f32>,
    eink_flash_count: Option<u32>,
    eink_reveal_portion: Option<f32>,
    eink_stripe_count: Option<u32>,
    eink_flash_color: Option<[u8; 3]>,
}

impl TransitionOptionBuilder {
    fn into_canonical_options(self, kind: TransitionKind) -> Result<Vec<TransitionOptions>> {
        match kind {
            TransitionKind::Wipe => {
                let angles = self.wipe_angle_list_deg.clone();
                let jitter = self.wipe_angle_jitter_deg;
                self.into_canonical_with_angles(kind, angles, jitter, |builder, base| {
                    builder.wipe_angle_list_deg = Some(vec![base])
                })
            }
            TransitionKind::Push => {
                let angles = self.push_angle_list_deg.clone();
                let jitter = self.push_angle_jitter_deg;
                self.into_canonical_with_angles(kind, angles, jitter, |builder, base| {
                    builder.push_angle_list_deg = Some(vec![base])
                })
            }
            _ => {
                let option = TransitionOptions::with_kind(kind, self)?;
                Ok(vec![option])
            }
        }
    }

    fn into_canonical_with_angles(
        self,
        kind: TransitionKind,
        angles: Option<Vec<f32>>,
        jitter: Option<f32>,
        mut apply_base: impl FnMut(&mut TransitionOptionBuilder, f32),
    ) -> Result<Vec<TransitionOptions>> {
        let jitter_value = jitter.unwrap_or(0.0);
        ensure!(
            jitter_value.is_finite(),
            format!(
                "transition option {} has non-finite angle-jitter-degrees",
                kind
            )
        );
        ensure!(
            jitter_value >= 0.0,
            format!(
                "transition option {} requires angle-jitter-degrees >= 0",
                kind
            )
        );
        let base_angles = angles.unwrap_or_else(|| vec![0.0]);
        ensure!(
            !base_angles.is_empty(),
            format!(
                "transition option {} requires angle-list-degrees to include at least one entry",
                kind
            )
        );
        let mut options = Vec::with_capacity(base_angles.len());
        let builder_template = self;
        for base in base_angles {
            ensure!(
                base.is_finite(),
                format!(
                    "transition option {} has non-finite values in angle-list-degrees",
                    kind
                )
            );
            let mut builder = builder_template.clone();
            apply_base(&mut builder, base);
            let option = TransitionOptions::with_kind(kind, builder)?;
            options.push(option);
        }
        Ok(options)
    }
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
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct Configuration {
    /// Root directory to scan recursively for images.
    pub photo_library_path: PathBuf,
    /// Unix domain socket accepting runtime control commands.
    #[serde(default = "Configuration::default_control_socket_path")]
    pub control_socket_path: PathBuf,
    /// Global photo render sizing/timing controls.
    pub global_photo_settings: GlobalPhotoSettings,
    /// Transition behavior between successive photos.
    pub transition: TransitionConfig,
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
    /// Sleep screen shown when the frame enters sleep mode.
    pub sleep_screen: SleepScreenConfig,
    /// Optional wake/sleep schedule used when a control daemon is absent.
    #[serde(default)]
    pub awake_schedule: Option<AwakeScheduleConfig>,
    /// Placeholder for the hardware button daemon's config block so that
    /// photo-frame can coexist with a shared config file without
    /// accepting other unknown keys.
    ///
    /// This crate does not read this field; it exists solely to reserve the
    /// `buttond` section and avoid serde `unknown field` errors due to
    /// `deny_unknown_fields` on `Configuration` when a shared config file is
    /// used across processes.
    #[serde(default)]
    #[allow(dead_code)] // reserved for external consumer (buttond service)
    pub buttond: Option<YamlValue>,
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
        ensure!(
            self.global_photo_settings.oversample > 0.0,
            "oversample must be positive"
        );
        ensure!(
            self.global_photo_settings.dwell_ms > 0,
            "dwell-ms must be greater than zero"
        );
        ensure!(
            !self.control_socket_path.as_os_str().is_empty(),
            "control-socket-path must not be empty"
        );
        ensure!(
            self.control_socket_path.file_name().is_some(),
            "control-socket-path must include a socket file name"
        );
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
        self.sleep_screen
            .validate()
            .context("invalid sleep screen configuration")?;
        if let Some(schedule) = self.awake_schedule.as_mut() {
            schedule
                .validate()
                .context("invalid awake schedule configuration")?;
        }
        Ok(self)
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            photo_library_path: PathBuf::new(),
            control_socket_path: Self::default_control_socket_path(),
            global_photo_settings: GlobalPhotoSettings::default(),
            transition: TransitionConfig::default(),
            viewer_preload_count: 3,
            loader_max_concurrent_decodes: 4,
            startup_shuffle_seed: None,
            photo_effect: PhotoEffectConfig::default(),
            matting: MattingConfig::default(),
            playlist: PlaylistOptions::default(),
            greeting_screen: GreetingScreenConfig::default(),
            sleep_screen: SleepScreenConfig::default(),
            awake_schedule: None,
            buttond: None,
        }
    }
}

impl Configuration {
    fn default_control_socket_path() -> PathBuf {
        PathBuf::from(DEFAULT_CONTROL_SOCKET_PATH)
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
