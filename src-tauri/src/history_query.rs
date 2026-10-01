use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    history::{HistoryError, HistoryRecord},
    model::{Provenance, ProviderId},
    platform::calendar::{Calendar, CalendarError, Day},
};

const GAP_SECONDS: i64 = 30 * 60;
const MAX_POINTS: usize = 100_000;
const MAX_DAYS: usize = 36_600;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitKey {
    pub id: String,
    pub window_seconds: Option<u64>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HistoryRange {
    Today {},
    Days7 {},
    Days30 {},
    AllTime {},
    Day { date: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub provider: ProviderId,
    pub account_key: String,
    pub limit: LimitKey,
    pub range: HistoryRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakReason {
    MissingTime,
    ResetChanged,
    ResetBoundary,
    UsageDecreased,
    LimitUnavailable,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Point {
    pub observed_at: i64,
    pub used_fraction: f64,
    pub resets_at: Option<i64>,
    pub break_before: Vec<BreakReason>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DailySummary {
    pub date: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub sample_count: u64,
    pub peak: Option<Point>,
}

impl From<Day> for DailySummary {
    fn from(day: Day) -> Self {
        Self {
            date: day.date,
            starts_at: day.starts_at,
            ends_at: day.ends_at,
            sample_count: 0,
            peak: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Observations {
    pub peak: Option<Point>,
    pub days_with_readings: usize,
    pub days_below_threshold: usize,
    pub days_in_range: usize,
    pub threshold_remaining: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueryResult {
    pub request: QueryRequest,
    pub timezone: String,
    pub from: i64,
    pub until: i64,
    pub days: Vec<DailySummary>,
    pub points: Vec<Point>,
    pub observations: Observations,
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error("Local history is unavailable. Try again after reopening Delta-V.")]
    Unavailable,
    #[error("History changed while it was being read. Try again.")]
    Cancelled,
    #[error("Choose a valid saved account and usage window.")]
    InvalidSelection,
    #[error("Choose a valid history period.")]
    InvalidRange,
    #[error(transparent)]
    Calendar(#[from] CalendarError),
    #[error("History readings are not in chronological order.")]
    UnorderedReadings,
    #[error("History contains different readings for the same account, window, and time.")]
    ConflictingReadings,
    #[error("A saved usage reading is invalid.")]
    InvalidReading,
    #[error("This history selection is too large to display. Choose a shorter period.")]
    TooLarge,
}

#[derive(Clone, PartialEq)]
struct Reading {
    used_fraction: f64,
    resets_at: Option<i64>,
}

struct Pending {
    observed_at: i64,
    reading: Option<Reading>,
}

pub struct Accumulator {
    request: QueryRequest,
    calendar: Calendar,
    from: Option<i64>,
    until: i64,
    today: Day,
    days: BTreeMap<i64, DailySummary>,
    points: Vec<Point>,
    intraday: bool,
    threshold_remaining: f64,
    pending: Option<Pending>,
    previous: Option<Point>,
    limit_unavailable: bool,
}

impl Accumulator {
    pub fn new(
        request: QueryRequest,
        calendar: Calendar,
        now: i64,
        threshold_remaining: f64,
    ) -> Result<Self, QueryError> {
        if request.account_key.len() != 64
            || !request
                .account_key
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || request.limit.id.trim().is_empty()
            || request.limit.id.len() > 256
            || request.limit.id.chars().any(char::is_control)
            || request.limit.window_seconds == Some(0)
        {
            return Err(QueryError::InvalidSelection);
        }
        if !threshold_remaining.is_finite() || !(0.0..=100.0).contains(&threshold_remaining) {
            return Err(QueryError::InvalidSelection);
        }
        let today = calendar.day(now)?;
        let mut until = now.checked_add(1).ok_or(QueryError::InvalidRange)?;
        let selected_days = match &request.range {
            HistoryRange::Today {} => vec![today.clone()],
            HistoryRange::Days7 {} => calendar.days_ending_at(now, 7)?,
            HistoryRange::Days30 {} => calendar.days_ending_at(now, 30)?,
            HistoryRange::AllTime {} => Vec::new(),
            HistoryRange::Day { date } => {
                let day = calendar
                    .day_for_date(date)
                    .map_err(|_| QueryError::InvalidRange)?;
                if day.starts_at > now {
                    return Err(QueryError::InvalidRange);
                }
                until = until.min(day.ends_at);
                vec![day]
            }
        };
        let from = selected_days.first().map(|day| day.starts_at);
        let days = selected_days
            .into_iter()
            .map(|day| (day.starts_at, day.into()))
            .collect();
        let intraday = matches!(
            request.range,
            HistoryRange::Today {} | HistoryRange::Day { .. }
        );
        Ok(Self {
            request,
            calendar,
            from,
            until,
            today,
            days,
            points: Vec::new(),
            intraday,
            threshold_remaining,
            pending: None,
            previous: None,
            limit_unavailable: false,
        })
    }

    pub fn reader_boundaries(&self) -> (Option<i64>, i64) {
        (self.from, self.until)
    }

    pub fn observe(&mut self, record: HistoryRecord) -> Result<(), QueryError> {
        if record.provider != self.request.provider
            || record.account_key != self.request.account_key
            || self.from.is_some_and(|from| record.observed_at < from)
            || record.observed_at >= self.until
        {
            return Ok(());
        }
        let mut selected = record.limits.iter().filter(|limit| {
            limit.id == self.request.limit.id
                && limit.window_seconds == self.request.limit.window_seconds
                && limit.provenance == self.request.limit.provenance
        });
        let reading = selected.next().map(|limit| Reading {
            used_fraction: limit.used_fraction,
            resets_at: limit.resets_at,
        });
        if selected.next().is_some()
            || reading.as_ref().is_some_and(|reading| {
                !reading.used_fraction.is_finite() || !(0.0..=1.0).contains(&reading.used_fraction)
            })
        {
            return Err(QueryError::InvalidReading);
        }
        if let Some(pending) = &self.pending {
            if record.observed_at < pending.observed_at {
                return Err(QueryError::UnorderedReadings);
            }
            if record.observed_at == pending.observed_at {
                return if reading == pending.reading {
                    Ok(())
                } else {
                    Err(QueryError::ConflictingReadings)
                };
            }
        }
        self.flush_pending()?;
        self.pending = Some(Pending {
            observed_at: record.observed_at,
            reading,
        });
        Ok(())
    }

    pub fn finish(mut self) -> Result<QueryResult, QueryError> {
        self.flush_pending()?;
        if matches!(self.request.range, HistoryRange::AllTime {})
            && let Some((&first, _)) = self.days.first_key_value()
        {
            let all_days = self.calendar.days(first, self.until)?;
            if all_days.len() > MAX_DAYS {
                return Err(QueryError::TooLarge);
            }
            self.from = Some(first);
            for day in all_days {
                self.days.entry(day.starts_at).or_insert_with(|| day.into());
            }
        }
        let days: Vec<DailySummary> = self.days.into_values().collect();
        let mut peak = None;
        let mut days_with_readings = 0;
        let mut days_below_threshold = 0;
        for day in &days {
            if let Some(point) = &day.peak {
                days_with_readings += 1;
                if point.used_fraction > (100.0 - self.threshold_remaining) / 100.0 {
                    days_below_threshold += 1;
                }
                update_peak(&mut peak, point);
            }
        }
        let observations = Observations {
            peak,
            days_with_readings,
            days_below_threshold,
            days_in_range: days.len(),
            threshold_remaining: self.threshold_remaining,
        };
        Ok(QueryResult {
            request: self.request,
            timezone: self.calendar.name().to_owned(),
            from: self.from.unwrap_or(self.today.starts_at),
            until: self.until,
            days,
            points: self.points,
            observations,
        })
    }

    fn flush_pending(&mut self) -> Result<(), QueryError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let Some(reading) = pending.reading else {
            self.limit_unavailable = true;
            return Ok(());
        };
        let mut point = Point {
            observed_at: pending.observed_at,
            used_fraction: reading.used_fraction,
            resets_at: reading.resets_at,
            break_before: Vec::new(),
        };
        if let Some(previous) = &self.previous {
            if point.observed_at.saturating_sub(previous.observed_at) > GAP_SECONDS {
                point.break_before.push(BreakReason::MissingTime);
            }
            if point.resets_at != previous.resets_at {
                point.break_before.push(BreakReason::ResetChanged);
            }
            if previous
                .resets_at
                .is_some_and(|reset| reset > previous.observed_at && reset <= point.observed_at)
            {
                point.break_before.push(BreakReason::ResetBoundary);
            }
            if point.used_fraction < previous.used_fraction {
                point.break_before.push(BreakReason::UsageDecreased);
            }
            if self.limit_unavailable {
                point.break_before.push(BreakReason::LimitUnavailable);
            }
        }
        let day_start = match self.days.range(..=point.observed_at).next_back() {
            Some((&start, day)) if point.observed_at < day.ends_at => start,
            _ => {
                if self.days.len() >= MAX_DAYS {
                    return Err(QueryError::TooLarge);
                }
                let day = self.calendar.day(point.observed_at)?;
                let start = day.starts_at;
                self.days.insert(start, day.into());
                start
            }
        };
        let summary = self
            .days
            .get_mut(&day_start)
            .ok_or(QueryError::InvalidRange)?;
        summary.sample_count += 1;
        update_peak(&mut summary.peak, &point);
        if self.intraday {
            if self.points.len() >= MAX_POINTS {
                return Err(QueryError::TooLarge);
            }
            self.points.push(point.clone());
        }
        self.previous = Some(point);
        self.limit_unavailable = false;
        Ok(())
    }
}

fn update_peak(peak: &mut Option<Point>, point: &Point) {
    if peak.as_ref().is_none_or(|current| {
        point.used_fraction > current.used_fraction
            || (point.used_fraction == current.used_fraction
                && point.observed_at < current.observed_at)
    }) {
        *peak = Some(point.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryLimit;
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};

    fn timestamp(value: &str) -> i64 {
        OffsetDateTime::parse(value, &Rfc3339)
            .unwrap()
            .unix_timestamp()
    }

    fn request(range: HistoryRange) -> QueryRequest {
        QueryRequest {
            provider: ProviderId::Claude,
            account_key: "a".repeat(64),
            limit: LimitKey {
                id: "five_hour".to_owned(),
                window_seconds: Some(18_000),
                provenance: Provenance::Official,
            },
            range,
        }
    }

    fn record(observed_at: i64, used_fraction: f64, resets_at: Option<i64>) -> HistoryRecord {
        HistoryRecord {
            version: 1,
            provider: ProviderId::Claude,
            account_key: "a".repeat(64),
            observed_at,
            limits: vec![HistoryLimit {
                id: "five_hour".to_owned(),
                label: "5-hour".to_owned(),
                used_fraction,
                resets_at,
                window_seconds: Some(18_000),
                provenance: Provenance::Official,
            }],
        }
    }

    fn accumulator(range: HistoryRange, now: i64, threshold: f64) -> Accumulator {
        Accumulator::new(
            request(range),
            Calendar::named("UTC").unwrap(),
            now,
            threshold,
        )
        .unwrap()
    }

    #[test]
    fn daily_extrema_use_observed_values_and_leave_unsampled_days_empty() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let first = timestamp("2026-09-25T08:00:00Z");
        let next_day = timestamp("2026-09-26T10:00:00Z");
        let mut query = accumulator(HistoryRange::Days7 {}, now, 20.0);
        query.observe(record(first, 0.3, None)).unwrap();
        query.observe(record(first + 60, 0.8, None)).unwrap();
        query.observe(record(first + 120, 0.8, None)).unwrap();
        query.observe(record(next_day, 0.81, None)).unwrap();
        let result = query.finish().unwrap();
        assert_eq!(result.days.len(), 7);
        assert_eq!(result.days[0].sample_count, 3);
        let peak = result.days[0].peak.as_ref().unwrap();
        assert_eq!(peak.used_fraction, 0.8);
        assert_eq!(peak.observed_at, first + 60);
        assert!(result.days[2..].iter().all(|day| day.peak.is_none()));
        assert!(result.points.is_empty());
        assert_eq!(result.observations.days_with_readings, 2);
        assert_eq!(result.observations.days_below_threshold, 1);
        assert_eq!(result.observations.days_in_range, 7);
        assert_eq!(result.observations.peak.unwrap().observed_at, next_day);
    }

    #[test]
    fn exact_thresholds_do_not_become_low_after_floating_point_subtraction() {
        let now = timestamp("2026-10-01T12:00:00Z");
        for threshold in 0..=100 {
            let mut query = accumulator(HistoryRange::Today {}, now, f64::from(threshold));
            query
                .observe(record(now, f64::from(100 - threshold) / 100.0, None))
                .unwrap();
            assert_eq!(query.finish().unwrap().observations.days_below_threshold, 0);
        }
    }

    #[test]
    fn accounts_providers_durations_and_provenance_are_separate() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let first = now - 600;
        let mut query = accumulator(HistoryRange::Today {}, now, 20.0);
        query.observe(record(first, 0.1, None)).unwrap();
        let mut other_account = record(first + 60, 0.99, None);
        other_account.account_key = "b".repeat(64);
        query.observe(other_account).unwrap();
        let mut other_provider = record(first + 120, 0.99, None);
        other_provider.provider = ProviderId::Codex;
        query.observe(other_provider).unwrap();
        query.observe(record(first + 180, 0.2, None)).unwrap();
        let mut other_duration = record(first + 240, 0.99, None);
        other_duration.limits[0].window_seconds = Some(604_800);
        query.observe(other_duration).unwrap();
        let mut estimate = record(first + 300, 0.99, None);
        estimate.limits[0].provenance = Provenance::LocalEstimate;
        query.observe(estimate).unwrap();
        query.observe(record(first + 360, 0.3, None)).unwrap();
        let result = query.finish().unwrap();
        assert_eq!(result.points.len(), 3);
        assert!(result.points[1].break_before.is_empty());
        assert_eq!(
            result.points[2].break_before,
            [BreakReason::LimitUnavailable]
        );
        assert_eq!(result.days[0].sample_count, 3);
        assert_eq!(result.observations.peak.unwrap().used_fraction, 0.3);
    }

    #[test]
    fn continuity_breaks_do_not_create_reset_or_missing_readings() {
        let first = timestamp("2026-10-01T08:00:00Z");
        let reset = Some(first + 600);
        let mut query = accumulator(HistoryRange::Today {}, first + 3600, 20.0);
        query.observe(record(first, 0.2, reset)).unwrap();
        query.observe(record(first + 60, 0.3, reset)).unwrap();
        let mut unavailable = record(first + 120, 0.9, reset);
        unavailable.limits.clear();
        query.observe(unavailable).unwrap();
        query.observe(record(first + 180, 0.4, reset)).unwrap();
        query.observe(record(first + 660, 0.5, reset)).unwrap();
        query.observe(record(first + 720, 0.1, None)).unwrap();
        query.observe(record(first + 2521, 0.2, None)).unwrap();
        let result = query.finish().unwrap();
        assert_eq!(result.points.len(), 6);
        assert!(result.points[0].break_before.is_empty());
        assert!(result.points[1].break_before.is_empty());
        assert_eq!(
            result.points[2].break_before,
            [BreakReason::LimitUnavailable]
        );
        assert_eq!(result.points[3].break_before, [BreakReason::ResetBoundary]);
        assert_eq!(
            result.points[4].break_before,
            [BreakReason::ResetChanged, BreakReason::UsageDecreased]
        );
        assert_eq!(result.points[5].break_before, [BreakReason::MissingTime]);
        assert!(result.points.iter().all(|point| point.used_fraction > 0.0));
        assert!(
            result
                .points
                .iter()
                .all(|point| point.observed_at != first + 600)
        );
    }

    #[test]
    fn repeated_identical_timestamps_count_once_but_conflicts_are_reported() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let mut query = accumulator(HistoryRange::Today {}, now, 20.0);
        query.observe(record(now, 0.4, None)).unwrap();
        let mut renamed = record(now, 0.4, None);
        renamed.limits[0].label = "Session window".to_owned();
        query.observe(renamed).unwrap();
        let result = query.finish().unwrap();
        assert_eq!(result.points.len(), 1);
        assert_eq!(result.days[0].sample_count, 1);
        let mut conflicting = accumulator(HistoryRange::Today {}, now, 20.0);
        conflicting.observe(record(now, 0.4, None)).unwrap();
        assert!(matches!(
            conflicting.observe(record(now, 0.5, None)),
            Err(QueryError::ConflictingReadings)
        ));
        let mut changed_reset = accumulator(HistoryRange::Today {}, now, 20.0);
        changed_reset.observe(record(now, 0.4, None)).unwrap();
        assert!(matches!(
            changed_reset.observe(record(now, 0.4, Some(now + 10))),
            Err(QueryError::ConflictingReadings)
        ));
    }

    #[test]
    fn unordered_input_is_explicitly_rejected_instead_of_changing_extrema() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let mut query = accumulator(HistoryRange::Today {}, now, 20.0);
        query.observe(record(now, 0.5, None)).unwrap();
        assert!(matches!(
            query.observe(record(now - 1, 0.4, None)),
            Err(QueryError::UnorderedReadings)
        ));
    }

    #[test]
    fn today_is_midnight_to_now_inclusive_and_never_includes_future_readings() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let start = timestamp("2026-10-01T00:00:00Z");
        let mut query = accumulator(HistoryRange::Today {}, now, 20.0);
        assert_eq!(query.reader_boundaries(), (Some(start), now + 1));
        query.observe(record(start - 1, 0.99, None)).unwrap();
        query.observe(record(start, 0.1, None)).unwrap();
        query.observe(record(now, 0.2, None)).unwrap();
        query.observe(record(now + 1, 0.99, None)).unwrap();
        let result = query.finish().unwrap();
        assert_eq!(result.points.len(), 2);
        assert_eq!(result.observations.peak.unwrap().used_fraction, 0.2);
    }

    #[test]
    fn all_time_starts_with_the_selected_series_and_keeps_later_empty_days() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let mut query = accumulator(HistoryRange::AllTime {}, now, 20.0);
        let mut unrelated = record(timestamp("2026-09-01T10:00:00Z"), 1.0, None);
        unrelated.limits[0].id = "seven_day".to_owned();
        query.observe(unrelated).unwrap();
        query
            .observe(record(timestamp("2026-09-29T10:00:00Z"), 0.7, None))
            .unwrap();
        query.observe(record(now, 0.2, None)).unwrap();
        let result = query.finish().unwrap();
        assert_eq!(result.from, timestamp("2026-09-29T00:00:00Z"));
        assert_eq!(result.days.len(), 3);
        assert_eq!(result.days[1].sample_count, 0);
        assert!(result.days[1].peak.is_none());
        assert!(result.points.is_empty());
        assert_eq!(result.observations.days_with_readings, 2);
        let empty = accumulator(HistoryRange::AllTime {}, now, 20.0)
            .finish()
            .unwrap();
        assert!(empty.days.is_empty());
        assert_eq!(empty.observations.days_in_range, 0);
        assert!(empty.observations.peak.is_none());
    }

    #[test]
    fn calendar_days_preserve_spring_dst_and_repeated_autumn_hours() {
        let mut spring = Accumulator::new(
            request(HistoryRange::Day {
                date: "2026-03-29".to_owned(),
            }),
            Calendar::named("Europe/Warsaw").unwrap(),
            timestamp("2026-03-30T12:00:00Z"),
            20.0,
        )
        .unwrap();
        let start = timestamp("2026-03-28T23:00:00Z");
        let end = timestamp("2026-03-29T22:00:00Z");
        assert_eq!(spring.reader_boundaries(), (Some(start), end));
        spring.observe(record(start, 0.1, None)).unwrap();
        spring.observe(record(end - 1, 0.2, None)).unwrap();
        spring.observe(record(end, 0.99, None)).unwrap();
        let spring = spring.finish().unwrap();
        assert_eq!(spring.days[0].ends_at - spring.days[0].starts_at, 23 * 3600);
        assert_eq!(spring.days[0].sample_count, 2);

        let mut autumn = Accumulator::new(
            request(HistoryRange::Day {
                date: "2026-10-25".to_owned(),
            }),
            Calendar::named("Europe/Warsaw").unwrap(),
            timestamp("2026-10-26T12:00:00Z"),
            20.0,
        )
        .unwrap();
        let first_hour = timestamp("2026-10-25T00:30:00Z");
        let repeated_hour = timestamp("2026-10-25T01:30:00Z");
        autumn.observe(record(first_hour, 0.1, None)).unwrap();
        autumn.observe(record(repeated_hour, 0.2, None)).unwrap();
        let autumn = autumn.finish().unwrap();
        assert_eq!(autumn.days[0].ends_at - autumn.days[0].starts_at, 25 * 3600);
        assert_eq!(autumn.points.len(), 2);
        assert_eq!(autumn.days[0].sample_count, 2);
        assert_eq!(
            autumn.days[0].peak.as_ref().unwrap().observed_at,
            repeated_hour
        );
        assert_eq!(autumn.timezone, "Europe/Warsaw");
    }

    #[test]
    fn invalid_selections_and_future_days_fail_before_reading_files() {
        let now = timestamp("2026-10-01T12:00:00Z");
        let mut invalid = request(HistoryRange::Today {});
        invalid.account_key = "someone@example.com".to_owned();
        assert!(matches!(
            Accumulator::new(invalid, Calendar::named("UTC").unwrap(), now, 20.0),
            Err(QueryError::InvalidSelection)
        ));
        assert!(matches!(
            Accumulator::new(
                request(HistoryRange::Day {
                    date: "2026-10-02".to_owned()
                }),
                Calendar::named("UTC").unwrap(),
                now,
                20.0
            ),
            Err(QueryError::InvalidRange)
        ));
        let mut invalid_reading = accumulator(HistoryRange::Today {}, now, 20.0);
        assert!(matches!(
            invalid_reading.observe(record(now, f64::NAN, None)),
            Err(QueryError::InvalidReading)
        ));
        assert!(
            serde_json::from_str::<HistoryRange>(r#"{"kind":"today","unrelated":true}"#).is_err()
        );
    }
}
