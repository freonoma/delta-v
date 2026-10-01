use serde::Serialize;
use tauri_nspanel::{
    objc2::rc::{Retained, autoreleasepool},
    objc2_foundation::{
        NSCalendar, NSCalendarIdentifierGregorian, NSCalendarUnit, NSDate, NSDateComponents,
        NSString, NSTimeZone,
    },
};
use thiserror::Error;
use time::{Date, Month, OffsetDateTime};

const MAX_DAYS: usize = 36_600;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Day {
    pub date: String,
    pub starts_at: i64,
    pub ends_at: i64,
}

#[derive(Debug, Error)]
pub enum CalendarError {
    #[error("macOS could not read the local time zone.")]
    Unavailable,
    #[error("The history date is outside the supported range.")]
    InvalidDate,
    #[error("The history date range is invalid.")]
    InvalidRange,
    #[error("The history date range exceeds 36,600 days.")]
    TooManyDays,
}

pub struct Calendar {
    native: Retained<NSCalendar>,
    name: String,
}

impl Calendar {
    pub fn local() -> Result<Self, CalendarError> {
        // Resolve the proxy to a named zone so one query cannot change zones halfway through.
        autoreleasepool(|_| Self::from_name(&NSTimeZone::localTimeZone().name().to_string()))
    }

    fn from_name(name: &str) -> Result<Self, CalendarError> {
        autoreleasepool(|_| {
            let zone = NSTimeZone::timeZoneWithName(&NSString::from_str(name))
                .ok_or(CalendarError::Unavailable)?;
            let native =
                NSCalendar::calendarWithIdentifier(unsafe { NSCalendarIdentifierGregorian })
                    .ok_or(CalendarError::Unavailable)?;
            native.setTimeZone(&zone);
            Ok(Self {
                native,
                name: zone.name().to_string(),
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn named(name: &str) -> Result<Self, CalendarError> {
        Self::from_name(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn day(&self, timestamp: i64) -> Result<Day, CalendarError> {
        // Worker threads have no AppKit event loop to drain Foundation's temporary objects.
        autoreleasepool(|_| {
            OffsetDateTime::from_unix_timestamp(timestamp)
                .map_err(|_| CalendarError::InvalidDate)?;
            let date = NSDate::dateWithTimeIntervalSince1970(timestamp as f64);
            let mut start = None;
            let mut interval = 0.0;
            // Foundation accounts for transitions at midnight as well as ordinary DST changes.
            let found = unsafe {
                self.native.rangeOfUnit_startDate_interval_forDate(
                    NSCalendarUnit::Day,
                    Some(&mut start),
                    &mut interval,
                    &date,
                )
            };
            if !found || !interval.is_finite() || interval <= 0.0 {
                return Err(CalendarError::InvalidDate);
            }
            let starts = start
                .ok_or(CalendarError::InvalidDate)?
                .timeIntervalSince1970();
            let starts_at = unix_seconds(starts)?;
            let ends_at = unix_seconds(starts + interval)?;
            if starts_at > timestamp || ends_at <= timestamp {
                return Err(CalendarError::InvalidDate);
            }
            let components = self.native.components_fromDate(
                NSCalendarUnit::Era
                    | NSCalendarUnit::Year
                    | NSCalendarUnit::Month
                    | NSCalendarUnit::Day,
                &date,
            );
            let year = i32::try_from(components.year()).map_err(|_| CalendarError::InvalidDate)?;
            let month = u8::try_from(components.month())
                .ok()
                .and_then(|month| Month::try_from(month).ok())
                .ok_or(CalendarError::InvalidDate)?;
            let day = u8::try_from(components.day()).map_err(|_| CalendarError::InvalidDate)?;
            if components.era() != 1 || !(1..=9999).contains(&year) {
                return Err(CalendarError::InvalidDate);
            }
            let date = Date::from_calendar_date(year, month, day)
                .map_err(|_| CalendarError::InvalidDate)?
                .to_string();
            Ok(Day {
                date,
                starts_at,
                ends_at,
            })
        })
    }

    pub fn day_for_date(&self, label: &str) -> Result<Day, CalendarError> {
        autoreleasepool(|_| {
            let bytes = label.as_bytes();
            if bytes.len() != 10
                || bytes[4] != b'-'
                || bytes[7] != b'-'
                || bytes
                    .iter()
                    .enumerate()
                    .any(|(index, value)| index != 4 && index != 7 && !value.is_ascii_digit())
            {
                return Err(CalendarError::InvalidDate);
            }
            let year = label[..4]
                .parse::<i32>()
                .map_err(|_| CalendarError::InvalidDate)?;
            let month = label[5..7]
                .parse::<u8>()
                .ok()
                .and_then(|month| Month::try_from(month).ok())
                .ok_or(CalendarError::InvalidDate)?;
            let day = label[8..]
                .parse::<u8>()
                .map_err(|_| CalendarError::InvalidDate)?;
            let parsed = Date::from_calendar_date(year, month, day)
                .map_err(|_| CalendarError::InvalidDate)?;
            if year < 1 || parsed.to_string() != label {
                return Err(CalendarError::InvalidDate);
            }
            let components = NSDateComponents::new();
            components.setYear(year as isize);
            components.setMonth(month as isize);
            components.setDay(day as isize);
            components.setHour(12);
            let date = self
                .native
                .dateFromComponents(&components)
                .ok_or(CalendarError::InvalidDate)?;
            let result = self.day(unix_seconds(date.timeIntervalSince1970())?)?;
            if result.date != label {
                return Err(CalendarError::InvalidDate);
            }
            Ok(result)
        })
    }

    pub fn days_ending_at(&self, now: i64, count: usize) -> Result<Vec<Day>, CalendarError> {
        if count == 0 {
            return Err(CalendarError::InvalidRange);
        }
        if count > MAX_DAYS {
            return Err(CalendarError::TooManyDays);
        }
        let mut days = Vec::with_capacity(count);
        let mut timestamp = now;
        for index in 0..count {
            let day = self.day(timestamp)?;
            if index + 1 < count {
                timestamp = day
                    .starts_at
                    .checked_sub(1)
                    .ok_or(CalendarError::InvalidDate)?;
            }
            days.push(day);
        }
        days.reverse();
        Ok(days)
    }

    pub fn days(&self, first: i64, end: i64) -> Result<Vec<Day>, CalendarError> {
        if end < first {
            return Err(CalendarError::InvalidRange);
        }
        OffsetDateTime::from_unix_timestamp(first).map_err(|_| CalendarError::InvalidDate)?;
        OffsetDateTime::from_unix_timestamp(end).map_err(|_| CalendarError::InvalidDate)?;
        let mut days = Vec::new();
        let mut timestamp = first;
        while timestamp < end {
            if days.len() == MAX_DAYS {
                return Err(CalendarError::TooManyDays);
            }
            let day = self.day(timestamp)?;
            timestamp = day.ends_at;
            days.push(day);
        }
        Ok(days)
    }
}

fn unix_seconds(value: f64) -> Result<i64, CalendarError> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(CalendarError::InvalidDate);
    }
    let timestamp = value as i64;
    OffsetDateTime::from_unix_timestamp(timestamp).map_err(|_| CalendarError::InvalidDate)?;
    Ok(timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description::well_known::Rfc3339;

    fn timestamp(value: &str) -> i64 {
        OffsetDateTime::parse(value, &Rfc3339)
            .unwrap()
            .unix_timestamp()
    }

    #[test]
    fn warsaw_days_follow_spring_and_autumn_clock_changes() {
        let calendar = Calendar::named("Europe/Warsaw").unwrap();
        let spring = calendar.day_for_date("2026-03-29").unwrap();
        assert_eq!(spring.starts_at, timestamp("2026-03-28T23:00:00Z"));
        assert_eq!(spring.ends_at, timestamp("2026-03-29T22:00:00Z"));
        assert_eq!(spring.ends_at - spring.starts_at, 23 * 3600);
        let autumn = calendar.day_for_date("2026-10-25").unwrap();
        assert_eq!(autumn.starts_at, timestamp("2026-10-24T22:00:00Z"));
        assert_eq!(autumn.ends_at, timestamp("2026-10-25T23:00:00Z"));
        assert_eq!(autumn.ends_at - autumn.starts_at, 25 * 3600);
        assert_eq!(calendar.name(), "Europe/Warsaw");
    }

    #[test]
    fn day_boundaries_are_half_open() {
        let calendar = Calendar::named("Europe/Warsaw").unwrap();
        let day = calendar.day_for_date("2026-03-29").unwrap();
        assert_eq!(calendar.day(day.starts_at).unwrap(), day);
        assert_eq!(calendar.day(day.ends_at - 1).unwrap(), day);
        assert_eq!(calendar.day(day.ends_at).unwrap().date, "2026-03-30");
        assert_eq!(calendar.days(day.starts_at, day.ends_at).unwrap(), [day]);
    }

    #[test]
    fn calendar_ranges_count_dates_instead_of_twenty_four_hour_periods() {
        let calendar = Calendar::named("Europe/Warsaw").unwrap();
        let now = timestamp("2026-03-30T12:00:00Z");
        let days = calendar.days_ending_at(now, 7).unwrap();
        assert_eq!(days.len(), 7);
        assert_eq!(days.first().unwrap().date, "2026-03-24");
        assert_eq!(days.last().unwrap().date, "2026-03-30");
        assert_eq!(days[5].ends_at - days[5].starts_at, 23 * 3600);
        assert!(
            days.windows(2)
                .all(|pair| pair[0].ends_at == pair[1].starts_at)
        );
    }

    #[test]
    fn fractional_hour_offsets_and_clock_changes_are_preserved() {
        let kolkata = Calendar::named("Asia/Kolkata").unwrap();
        let day = kolkata.day_for_date("2026-10-01").unwrap();
        assert_eq!(day.starts_at, timestamp("2026-09-30T18:30:00Z"));
        assert_eq!(day.ends_at - day.starts_at, 24 * 3600);
        let lord_howe = Calendar::named("Australia/Lord_Howe").unwrap();
        let day = lord_howe.day_for_date("2026-10-04").unwrap();
        assert_eq!(day.ends_at - day.starts_at, 23 * 3600 + 1800);
    }

    #[test]
    fn midnight_transitions_and_skipped_dates_do_not_create_fake_days() {
        let sao_paulo = Calendar::named("America/Sao_Paulo").unwrap();
        let day = sao_paulo.day_for_date("2018-11-04").unwrap();
        assert_eq!(day.starts_at, timestamp("2018-11-04T03:00:00Z"));
        assert_eq!(day.ends_at, timestamp("2018-11-05T02:00:00Z"));
        let apia = Calendar::named("Pacific/Apia").unwrap();
        assert!(apia.day_for_date("2011-12-30").is_err());
        let days = apia
            .days_ending_at(timestamp("2011-12-31T12:00:00+14:00"), 2)
            .unwrap();
        assert_eq!(days[0].date, "2011-12-29");
        assert_eq!(days[1].date, "2011-12-31");
        assert_eq!(days[0].ends_at, days[1].starts_at);
    }

    #[test]
    fn invalid_dates_and_unbounded_ranges_are_rejected() {
        let calendar = Calendar::named("UTC").unwrap();
        for label in [
            "2026-02-29",
            "2026-2-01",
            "2026-13-01",
            "0000-01-01",
            "not a date",
        ] {
            assert!(calendar.day_for_date(label).is_err(), "{label}");
        }
        assert!(calendar.day_for_date("2024-02-29").is_ok());
        assert!(calendar.day(i64::MAX).is_err());
        assert!(calendar.day(i64::MIN).is_err());
        assert!(calendar.days(1, 0).is_err());
        assert!(calendar.days(0, 0).unwrap().is_empty());
        assert!(calendar.days_ending_at(0, 0).is_err());
        assert!(matches!(
            calendar.days_ending_at(0, MAX_DAYS + 1),
            Err(CalendarError::TooManyDays)
        ));
    }
}
