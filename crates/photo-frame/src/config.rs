use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime,
    NaiveTime, TimeZone, Weekday,
};
use chrono_tz::Tz;
use rand::Rng;
use rand::seq::IteratorRandom;
use serde::Deserialize;
use serde::de::{self, DeserializeOwned, Deserializer, MapAccess, SeqAccess, Unexpected, Visitor};
use serde_yaml::{Mapping, Value as YamlValue};

use crate::processing::fixed_image::FixedImageBackground;

pub const DEFAULT_CONTROL_SOCKET_PATH: &str = "/run/photo-frame/control.sock";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct GreetingScreenColorsConfig {
    pub background: Option<String>,
    pub font: Option<String>,
    pub accent: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct ScreenMessageConfig {
    pub message: Option<String>,
    pub font: Option<String>,
    pub stroke_width: Option<f32>,
    pub corner_radius: Option<f32>,
    #[serde(default)]
    pub colors: GreetingScreenColorsConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct GreetingScreenConfig {
    #[serde(flatten)]
    pub screen: ScreenMessageConfig,
    pub duration_seconds: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct SleepScreenConfig {
    #[serde(flatten)]
    pub screen: ScreenMessageConfig,
}

impl ScreenMessageConfig {
    const DEFAULT_STROKE_WIDTH_DIP: f32 = 16.0;

    pub fn message_or_default(&self) -> std::borrow::Cow<'_, str> {
        match &self.message {
            Some(msg) if !msg.trim().is_empty() => std::borrow::Cow::Borrowed(msg.as_str()),
            _ => std::borrow::Cow::Borrowed("Initializing…"),
        }
    }

    pub fn effective_stroke_width_dip(&self) -> f32 {
        let width = self
            .stroke_width
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(Self::DEFAULT_STROKE_WIDTH_DIP);
        width.max(0.1)
    }

    pub fn effective_corner_radius_dip(&self, default_stroke: f32) -> f32 {
        let radius = self
            .corner_radius
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or_else(|| {
                let base = self
                    .stroke_width
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(default_stroke);
                base * 0.75
            });
        radius.max(0.0)
    }

    fn validate(&self, prefix: &str) -> Result<()> {
        if let Some(width) = self.stroke_width {
            ensure!(
                width.is_finite() && width > 0.0,
                "{}.stroke-width must be positive",
                prefix
            );
        }
        if let Some(radius) = self.corner_radius {
            ensure!(
                radius.is_finite() && radius >= 0.0,
                "{}.corner-radius must be non-negative",
                prefix
            );
        }
        if let Some(font_name) = &self.font {
            ensure!(
                !font_name.trim().is_empty(),
                "{}.font must not be blank when provided",
                prefix
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
                    "{}.colors.{} must not be blank when provided",
                    prefix,
                    field
                );
            }
        }
        Ok(())
    }
}

impl GreetingScreenConfig {
    const DEFAULT_DURATION_SECONDS: f32 = 4.0;

    pub fn effective_duration(&self) -> Duration {
        let seconds = self
            .duration_seconds
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(Self::DEFAULT_DURATION_SECONDS)
            .max(0.0);
        Duration::from_secs_f32(seconds)
    }

    pub fn validate(&self) -> Result<()> {
        self.screen.validate("greeting-screen")?;
        if let Some(duration) = self.duration_seconds {
            ensure!(
                duration.is_finite() && duration >= 0.0,
                "greeting-screen.duration-seconds must be non-negative"
            );
        }
        Ok(())
    }

    pub fn screen(&self) -> &ScreenMessageConfig {
        &self.screen
    }
}

impl SleepScreenConfig {
    pub fn validate(&self) -> Result<()> {
        self.screen.validate("sleep-screen")
    }

    pub fn screen(&self) -> &ScreenMessageConfig {
        &self.screen
    }
}

impl Default for SleepScreenConfig {
    fn default() -> Self {
        let mut screen = ScreenMessageConfig::default();
        screen.message = Some("Going to Sleep".to_string());
        Self { screen }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AwakeScheduleConfig {
    pub timezone: Tz,
    #[serde(rename = "awake-scheduled", default)]
    schedule: AwakeScheduleRules,
}

impl AwakeScheduleConfig {
    pub fn validate(&mut self) -> Result<()> {
        self.schedule.validate()
    }

    pub fn timezone(&self) -> Tz {
        self.timezone
    }

    pub fn is_awake_at(&self, instant: DateTime<Tz>) -> bool {
        self.intervals_for_date(instant.date_naive())
            .into_iter()
            .any(|interval| interval.contains(instant))
    }

    pub fn next_transition_after(&self, from: DateTime<Tz>) -> Option<(DateTime<Tz>, bool)> {
        let start_date = from.date_naive();
        for offset in 0..=7 {
            let offset_days = i64::try_from(offset).ok()?;
            let date = start_date + ChronoDuration::days(offset_days);
            for interval in self.intervals_for_date(date) {
                if interval.start > from {
                    return Some((interval.start, true));
                }
                if interval.end > from {
                    return Some((interval.end, false));
                }
            }
        }
        None
    }

    fn intervals_for_date(&self, date: NaiveDate) -> Vec<ResolvedAwakeInterval> {
        let mut intervals = Vec::new();
        for range in self.schedule.resolved_ranges_for(date.weekday()) {
            let start = resolve_local_datetime(self.timezone, date, range.start(), Boundary::Start);
            let end = resolve_local_datetime(self.timezone, date, range.end(), Boundary::End);
            if end > start {
                intervals.push(ResolvedAwakeInterval { start, end });
            }
        }
        intervals
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
struct AwakeScheduleRules {
    #[serde(default)]
    daily: Vec<AwakeTimeRange>,
    weekdays: Option<Vec<AwakeTimeRange>>,
    weekend: Option<Vec<AwakeTimeRange>>,
    monday: Option<Vec<AwakeTimeRange>>,
    tuesday: Option<Vec<AwakeTimeRange>>,
    wednesday: Option<Vec<AwakeTimeRange>>,
    thursday: Option<Vec<AwakeTimeRange>>,
    friday: Option<Vec<AwakeTimeRange>>,
    saturday: Option<Vec<AwakeTimeRange>>,
    sunday: Option<Vec<AwakeTimeRange>>,
}

impl AwakeScheduleRules {
    fn validate(&mut self) -> Result<()> {
        Self::validate_ranges(&mut self.daily, "awake-schedule.awake-scheduled.daily")?;
        if let Some(ranges) = self.weekdays.as_mut() {
            Self::validate_ranges(ranges, "awake-schedule.awake-scheduled.weekdays")?;
        }
        if let Some(ranges) = self.weekend.as_mut() {
            Self::validate_ranges(ranges, "awake-schedule.awake-scheduled.weekend")?;
        }
        for (label, ranges) in [
            ("monday", &mut self.monday),
            ("tuesday", &mut self.tuesday),
            ("wednesday", &mut self.wednesday),
            ("thursday", &mut self.thursday),
            ("friday", &mut self.friday),
            ("saturday", &mut self.saturday),
            ("sunday", &mut self.sunday),
        ] {
            if let Some(ranges) = ranges {
                Self::validate_ranges(ranges, &format!("awake-schedule.awake-scheduled.{label}"))?;
            }
        }
        Ok(())
    }

    fn validate_ranges(ranges: &mut Vec<AwakeTimeRange>, label: &str) -> Result<()> {
        ranges.sort_unstable_by_key(|range| range.start());
        let mut previous_end: Option<NaiveTime> = None;
        for range in ranges.iter() {
            if let Some(prev) = previous_end {
                ensure!(
                    range.start() >= prev,
                    "{} intervals must not overlap",
                    label
                );
            }
            previous_end = Some(range.end());
        }
        Ok(())
    }

    fn resolved_ranges_for(&self, weekday: Weekday) -> Vec<AwakeTimeRange> {
        if let Some(overrides) = self.day_specific(weekday) {
            return overrides.clone();
        }
        match weekday {
            Weekday::Sat | Weekday::Sun => {
                self.weekend.clone().unwrap_or_else(|| self.daily.clone())
            }
            _ => self.weekdays.clone().unwrap_or_else(|| self.daily.clone()),
        }
    }

    fn day_specific(&self, weekday: Weekday) -> Option<&Vec<AwakeTimeRange>> {
        match weekday {
            Weekday::Mon => self.monday.as_ref(),
            Weekday::Tue => self.tuesday.as_ref(),
            Weekday::Wed => self.wednesday.as_ref(),
            Weekday::Thu => self.thursday.as_ref(),
            Weekday::Fri => self.friday.as_ref(),
            Weekday::Sat => self.saturday.as_ref(),
            Weekday::Sun => self.sunday.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AwakeTimeRange {
    start: NaiveTime,
    end: NaiveTime,
}

impl AwakeTimeRange {
    fn start(&self) -> NaiveTime {
        self.start
    }

    fn end(&self) -> NaiveTime {
        self.end
    }
}

impl<'de> Deserialize<'de> for AwakeTimeRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (start_raw, end_raw): (String, String) = Deserialize::deserialize(deserializer)?;
        let parse_time = |value: &str| -> Result<NaiveTime, D::Error> {
            let trimmed = value.trim();
            for format in ["%H:%M:%S", "%H:%M"] {
                if let Ok(parsed) = NaiveTime::parse_from_str(trimmed, format) {
                    return Ok(parsed);
                }
            }
            Err(de::Error::custom(format!("invalid time literal '{value}'")))
        };
        let start = parse_time(&start_raw)?;
        let end = parse_time(&end_raw)?;
        if start >= end {
            return Err(de::Error::custom(format!(
                "awake interval must have start < end (start={start_raw}, end={end_raw})"
            )));
        }
        Ok(Self { start, end })
    }
}

#[derive(Debug, Clone)]
struct ResolvedAwakeInterval {
    start: DateTime<Tz>,
    end: DateTime<Tz>,
}

impl ResolvedAwakeInterval {
    fn contains(&self, instant: DateTime<Tz>) -> bool {
        instant >= self.start && instant < self.end
    }
}

#[derive(Debug, Clone, Copy)]
enum Boundary {
    Start,
    End,
}

fn resolve_local_datetime(
    tz: Tz,
    date: NaiveDate,
    time: NaiveTime,
    boundary: Boundary,
) -> DateTime<Tz> {
    let mut candidate = NaiveDateTime::new(date, time);
    for _ in 0..=180 {
        match tz.from_local_datetime(&candidate) {
            LocalResult::Single(dt) => return dt,
            LocalResult::Ambiguous(earliest, latest) => {
                return match boundary {
                    Boundary::Start => earliest,
                    Boundary::End => latest,
                };
            }
            LocalResult::None => {
                candidate += ChronoDuration::minutes(1);
            }
        }
    }
    tz.from_local_datetime(&candidate)
        .earliest()
        .expect("failed to resolve local datetime after DST adjustment")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule_from_yaml(input: &str) -> AwakeScheduleConfig {
        let mut schedule: AwakeScheduleConfig = serde_yaml::from_str(input).expect("valid yaml");
        schedule.validate().expect("valid schedule");
        schedule
    }

    #[test]
    fn next_transition_from_daily_interval() {
        let schedule = schedule_from_yaml(
            r#"
timezone: "UTC"
awake-scheduled:
  daily:
    - ["07:00", "09:30"]
"#,
        );

        let tz = schedule.timezone();
        let before = tz.with_ymd_and_hms(2024, 1, 1, 6, 0, 0).single().unwrap();
        let (start, awake) = schedule.next_transition_after(before).expect("transition");
        assert!(awake);
        assert_eq!(
            start,
            tz.with_ymd_and_hms(2024, 1, 1, 7, 0, 0).single().unwrap()
        );

        let during = tz.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).single().unwrap();
        assert!(schedule.is_awake_at(during));
        let (end, awake) = schedule
            .next_transition_after(during)
            .expect("end transition");
        assert!(!awake);
        assert_eq!(
            end,
            tz.with_ymd_and_hms(2024, 1, 1, 9, 30, 0).single().unwrap()
        );
    }

    #[test]
    fn day_specific_overrides_weekdays() {
        let schedule = schedule_from_yaml(
            r#"
timezone: "America/New_York"
awake-scheduled:
  daily:
    - ["07:00", "21:00"]
  weekdays:
    - ["06:00", "22:00"]
  friday: []
  saturday:
    - ["09:00", "12:00"]
"#,
        );

        let tz = schedule.timezone();
        let thursday = tz.with_ymd_and_hms(2024, 7, 18, 6, 30, 0).single().unwrap();
        assert!(schedule.is_awake_at(thursday));

        let friday = tz.with_ymd_and_hms(2024, 7, 19, 12, 0, 0).single().unwrap();
        assert!(!schedule.is_awake_at(friday));

        let saturday = tz
            .with_ymd_and_hms(2024, 7, 20, 10, 30, 0)
            .single()
            .unwrap();
        assert!(schedule.is_awake_at(saturday));
    }

    #[test]
    fn dst_gap_shifts_to_next_valid_time() {
        let schedule = schedule_from_yaml(
            r#"
timezone: "America/New_York"
awake-scheduled:
  sunday:
    - ["02:00", "03:30"]
"#,
        );

        let tz = schedule.timezone();
        let before = tz.with_ymd_and_hms(2024, 3, 10, 1, 0, 0).single().unwrap();
        let (start, awake) = schedule
            .next_transition_after(before)
            .expect("start transition");
        assert!(awake);
        assert_eq!(
            start,
            tz.with_ymd_and_hms(2024, 3, 10, 3, 0, 0).single().unwrap()
        );

        let (end, awake) = schedule
            .next_transition_after(start)
            .expect("end transition");
        assert!(!awake);
        assert_eq!(
            end,
            tz.with_ymd_and_hms(2024, 3, 10, 3, 30, 0).single().unwrap()
        );
    }

    #[test]
    fn dst_repeat_uses_latest_end() {
        let schedule = schedule_from_yaml(
            r#"
timezone: "America/New_York"
awake-scheduled:
  sunday:
    - ["01:00", "02:00"]
"#,
        );

        let tz = schedule.timezone();
        let base = tz.with_ymd_and_hms(2024, 11, 3, 0, 30, 0).single().unwrap();
        let (start, awake) = schedule.next_transition_after(base).expect("start");
        assert!(awake);
        let expected_start = tz
            .with_ymd_and_hms(2024, 11, 3, 1, 0, 0)
            .earliest()
            .unwrap();
        assert_eq!(start, expected_start);

        let (end, awake) = schedule.next_transition_after(start).expect("end");
        assert!(!awake);
        let expected_end = tz.with_ymd_and_hms(2024, 11, 3, 2, 0, 0).latest().unwrap();
        assert_eq!(end, expected_end);
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

struct SelectedIter<'a, K: Copy + Ord, O> {
    entries: SelectionEntries<'a, K>,
    options: &'a BTreeMap<K, O>,
}

impl<'a, K: Copy + Ord, O> SelectedIter<'a, K, O> {
    fn new(entries: SelectionEntries<'a, K>, options: &'a BTreeMap<K, O>) -> Self {
        Self { entries, options }
    }
}

impl<'a, K: Copy + Ord, O> Iterator for SelectedIter<'a, K, O> {
    type Item = SelectedOption<'a, K, O>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(entry) = self.entries.next() {
            if let Some(option) = self.options.get(&entry.kind) {
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
                match FixedImageBackground::new(path.clone()) {
                    Ok(background) => backgrounds.push(Arc::new(background)),
                    Err(err) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %err,
                            "skipping fixed background image that failed to prepare"
                        );
                    }
                }
            }

            if backgrounds.is_empty() {
                tracing::warn!(
                    "all configured fixed-image backgrounds failed to load; disabling fixed-image matting"
                );
            } else {
                self.runtime.fixed_image =
                    Some(FixedImageRuntime::new(backgrounds, *path_selection));
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
                "color" => {
                    if builder.fixed_colors.is_some() {
                        return Err(de::Error::duplicate_field("color"));
                    }
                    let color = inline_value_to::<[u8; 3], E>(value)?;
                    builder.fixed_colors = Some(vec![color]);
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
                            "color",
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
        let resolved_selection =
            resolve_pipeline_selection::<A::Error>(selection, active_entries.len(), "matting")?;

        let mut options = BTreeMap::new();
        let mut entries = Vec::with_capacity(active_entries.len());
        for (index, entry) in active_entries.into_iter().enumerate() {
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
            let option = MattingOptions::with_kind(kind, builder);
            options.insert(kind, option);
            entries.push(SelectionEntry { index, kind });
        }

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
    pub fn options(&self) -> &BTreeMap<MattingKind, MattingOptions> {
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
        SelectedIter::new(self.selection_entries(), &self.options)
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
            .get(&entry.kind)
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
        for option in self.options.values_mut() {
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
            PhotoEffectSelection::Fixed(entry) => self.options.get(&entry.kind).cloned(),
            PhotoEffectSelection::Random(entries) => entries
                .iter()
                .copied()
                .choose(rng)
                .and_then(|entry| self.options.get(&entry.kind).cloned()),
            PhotoEffectSelection::Sequential { entries, runtime } => {
                if entries.is_empty() {
                    None
                } else {
                    let index = runtime.next(entries.len());
                    let entry = entries[index];
                    self.options.get(&entry.kind).cloned()
                }
            }
        }
    }

    pub fn prepare_runtime(&mut self) -> Result<()> {
        match &self.selection {
            PhotoEffectSelection::Disabled => return Ok(()),
            PhotoEffectSelection::Fixed(entry) => {
                ensure!(
                    self.options.contains_key(&entry.kind),
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
                        self.options.contains_key(&entry.kind),
                        "photo-effect.active entry {} must define options for {}",
                        entry.index,
                        entry.kind,
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
                options: BTreeMap::new(),
            });
        }

        let resolved_selection = resolve_pipeline_selection::<A::Error>(
            selection,
            active_entries.len(),
            "photo-effect",
        )?;

        let mut options = BTreeMap::new();
        let mut entries = Vec::with_capacity(active_entries.len());
        for (index, entry) in active_entries.into_iter().enumerate() {
            let kind = entry.kind;
            let option = build_photo_effect_option::<A::Error>(kind, entry.fields)?;
            options.insert(kind, option);
            entries.push(SelectionEntry { index, kind });
        }

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
    Iris,
}

impl TransitionKind {
    const ALL: &'static [Self] = &[Self::Fade, Self::Wipe, Self::Push, Self::EInk, Self::Iris];
    const NAMES: &'static [&'static str] = &["fade", "wipe", "push", "e-ink", "iris"];

    fn as_str(&self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::Wipe => "wipe",
            Self::Push => "push",
            Self::EInk => "e-ink",
            Self::Iris => "iris",
        }
    }

    pub const fn as_index(&self) -> u32 {
        match self {
            Self::Fade => 1,
            Self::Wipe => 2,
            Self::Push => 3,
            Self::EInk => 4,
            Self::Iris => 5,
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
    pub fn options(&self) -> &BTreeMap<TransitionKind, TransitionOptions> {
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
        SelectedIter::new(self.selection_entries(), &self.options)
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
            .get(&entry.kind)
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
        for option in self.options.values_mut() {
            option.normalize()?;
        }
        match &self.selection {
            TransitionSelection::Fixed(entry) => ensure!(
                self.options.contains_key(&entry.kind),
                format!(
                    "transition.active entry {} must resolve to a configured option",
                    entry.kind
                )
            ),
            TransitionSelection::Random(entries)
            | TransitionSelection::Sequential { entries, .. } => {
                for entry in entries.iter() {
                    ensure!(
                        self.options.contains_key(&entry.kind),
                        "transition.active entry {} must resolve to a configured option",
                        entry.kind
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
            TransitionKind::Iris => (900, TransitionMode::Iris(IrisTransition::default())),
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
            TransitionMode::Iris(iris) => {
                iris.normalize(self.kind)?;
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
            TransitionKind::Iris => {
                let defaults = IrisTransition::default();
                TransitionMode::Iris(IrisTransition {
                    blades: builder.iris_blades.unwrap_or(defaults.blades),
                    direction: builder.iris_direction.unwrap_or(defaults.direction),
                    line_rgba: builder.iris_line_rgba.unwrap_or(defaults.line_rgba),
                    arc_rgba: builder.iris_arc_rgba.unwrap_or(defaults.arc_rgba),
                    line_thickness_px: builder
                        .iris_line_thickness_px
                        .unwrap_or(defaults.line_thickness_px),
                    taper: builder.iris_taper.unwrap_or(defaults.taper),
                    vignette: builder.iris_vignette.unwrap_or(defaults.vignette),
                    easing: builder.iris_easing.unwrap_or(defaults.easing),
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
            TransitionMode::Iris(iris) => {
                iris.normalize(kind)?;
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
    Iris(IrisTransition),
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
    pub angles_deg: Arc<[f32]>,
    pub selection: AngleSelection,
    pub jitter_deg: f32,
    runtime: AngleSequenceState,
}

impl Default for AnglePicker {
    fn default() -> Self {
        Self {
            angles_deg: Arc::from([0.0_f32]),
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
            angles_deg: Arc::from(angles_deg.unwrap_or_else(|| vec![0.0])),
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
        for angle in self.angles_deg.iter() {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IrisDirection {
    Open,
    Close,
}

impl Default for IrisDirection {
    fn default() -> Self {
        Self::Open
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IrisEasing {
    Linear,
    Cubic,
}

impl Default for IrisEasing {
    fn default() -> Self {
        Self::Cubic
    }
}

#[derive(Debug, Clone)]
pub struct IrisTransition {
    pub blades: u32,
    pub direction: IrisDirection,
    pub line_rgba: [f32; 4],
    pub arc_rgba: [f32; 4],
    pub line_thickness_px: f32,
    pub taper: f32,
    pub vignette: f32,
    pub easing: IrisEasing,
}

impl Default for IrisTransition {
    fn default() -> Self {
        Self {
            blades: 7,
            direction: IrisDirection::default(),
            line_rgba: [0.95, 0.95, 0.95, 0.35],
            arc_rgba: [0.95, 0.95, 0.95, 0.20],
            line_thickness_px: 2.0,
            taper: 0.6,
            vignette: 0.2,
            easing: IrisEasing::default(),
        }
    }
}

impl IrisTransition {
    fn normalize(&mut self, kind: TransitionKind) -> Result<()> {
        if self.blades < 5 {
            self.blades = 5;
        }
        if self.blades > 18 {
            self.blades = 18;
        }
        ensure!(
            self.line_thickness_px.is_finite(),
            format!(
                "transition option {} has non-finite iris.line-thickness-px",
                kind
            )
        );
        if self.line_thickness_px < 0.0 {
            self.line_thickness_px = 0.0;
        }
        ensure!(
            self.taper.is_finite(),
            format!("transition option {} has non-finite iris.taper", kind)
        );
        self.taper = self.taper.clamp(0.0, 1.0);
        ensure!(
            self.vignette.is_finite(),
            format!("transition option {} has non-finite iris.vignette", kind)
        );
        self.vignette = self.vignette.clamp(0.0, 1.0);
        for (idx, channel) in self.line_rgba.iter_mut().enumerate() {
            ensure!(
                channel.is_finite(),
                format!(
                    "transition option {} has non-finite iris.line-rgba[{}]",
                    kind, idx
                )
            );
            *channel = channel.clamp(0.0, 1.0);
        }
        for (idx, channel) in self.arc_rgba.iter_mut().enumerate() {
            ensure!(
                channel.is_finite(),
                format!(
                    "transition option {} has non-finite iris.arc-rgba[{}]",
                    kind, idx
                )
            );
            *channel = channel.clamp(0.0, 1.0);
        }
        Ok(())
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
        let resolved_selection =
            resolve_pipeline_selection::<A::Error>(selection, active_entries.len(), "transition")?;

        let mut options = BTreeMap::new();
        let mut entries = Vec::with_capacity(active_entries.len());
        for (index, entry) in active_entries.into_iter().enumerate() {
            let kind = entry.kind;
            let mut builder = TransitionOptionBuilder::default();
            for (field, value) in entry.fields {
                apply_transition_inline_field::<A::Error>(&mut builder, kind, &field, value)?;
            }
            let option = TransitionOptions::with_kind(kind, builder)
                .map_err(|err| de::Error::custom(err.to_string()))?;
            options.insert(kind, option);
            entries.push(SelectionEntry { index, kind });
        }

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
    iris_blades: Option<u32>,
    iris_direction: Option<IrisDirection>,
    iris_line_rgba: Option<[f32; 4]>,
    iris_arc_rgba: Option<[f32; 4]>,
    iris_line_thickness_px: Option<f32>,
    iris_taper: Option<f32>,
    iris_vignette: Option<f32>,
    iris_easing: Option<IrisEasing>,
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
        "blades" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_blades = Some(inline_value_to::<u32, E>(value)?);
        }
        "direction" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_direction = Some(inline_value_to::<IrisDirection, E>(value)?);
        }
        "line-rgba" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_line_rgba = Some(inline_value_to::<[f32; 4], E>(value)?);
        }
        "arc-rgba" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_arc_rgba = Some(inline_value_to::<[f32; 4], E>(value)?);
        }
        "line-thickness-px" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_line_thickness_px = Some(inline_value_to::<f32, E>(value)?);
        }
        "taper" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_taper = Some(inline_value_to::<f32, E>(value)?);
        }
        "vignette" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_vignette = Some(inline_value_to::<f32, E>(value)?);
        }
        "easing" if matches!(kind, TransitionKind::Iris) => {
            builder.iris_easing = Some(inline_value_to::<IrisEasing, E>(value)?);
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
                    "blades",
                    "direction",
                    "line-rgba",
                    "arc-rgba",
                    "line-thickness-px",
                    "taper",
                    "vignette",
                    "easing",
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
    /// Unix domain socket accepting runtime control commands.
    #[serde(default = "Configuration::default_control_socket_path")]
    pub control_socket_path: PathBuf,
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
    /// Sleep screen shown when the frame enters sleep mode.
    pub sleep_screen: SleepScreenConfig,
    /// Optional scheduled awake intervals that toggle viewer state automatically.
    pub awake_schedule: Option<AwakeScheduleConfig>,
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
            sleep_screen: SleepScreenConfig::default(),
            awake_schedule: None,
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
