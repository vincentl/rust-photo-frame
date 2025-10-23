use std::borrow::Cow;
use std::time::Duration;

use anyhow::{Result, ensure};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveDate, NaiveDateTime,
    NaiveTime, TimeZone, Weekday,
};
use chrono_tz::Tz;
use serde::Deserialize;
use serde::de::{self, Deserializer};

pub use awake::{AwakeScheduleConfig, AwakeScheduleRules, AwakeTimeRange};
pub use greeting::{
    GreetingScreenColorsConfig, GreetingScreenConfig, ScreenMessageConfig, SleepScreenConfig,
};

mod greeting {
    use super::*;

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

        pub fn message_or_default(&self) -> Cow<'_, str> {
            match &self.message {
                Some(msg) if !msg.trim().is_empty() => Cow::Borrowed(msg.as_str()),
                _ => Cow::Borrowed("Initializing…"),
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

        pub fn validate(&self, prefix: &str) -> Result<()> {
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
}

mod awake {
    use super::*;

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
                let start =
                    resolve_local_datetime(self.timezone, date, range.start(), Boundary::Start);
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
    pub struct AwakeScheduleRules {
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
        pub fn validate(&mut self) -> Result<()> {
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
                    Self::validate_ranges(
                        ranges,
                        &format!("awake-schedule.awake-scheduled.{label}"),
                    )?;
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
    pub struct AwakeTimeRange {
        start: NaiveTime,
        end: NaiveTime,
    }

    impl AwakeTimeRange {
        pub fn start(&self) -> NaiveTime {
            self.start
        }

        pub fn end(&self) -> NaiveTime {
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
