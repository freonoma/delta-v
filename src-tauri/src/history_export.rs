use serde::Deserialize;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    history_query::{BreakReason, QueryError, QueryRequest, QueryResult},
    model::{Provenance, ProviderId},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const HEADER: &str = "observed_at_utc,timezone,local_date,provider,account_key,limit_id,window_seconds,used_fraction,remaining_fraction,resets_at_utc,provenance,break_before\r\n";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    pub request: QueryRequest,
    pub timezone: String,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
}

impl ExportRequest {
    pub fn check_scope(&self, result: &QueryResult) -> Result<(), ExportError> {
        if self.timezone != result.timezone
            || self.first_date.as_deref() != result.days.first().map(|day| day.date.as_str())
            || self.last_date.as_deref() != result.days.last().map(|day| day.date.as_str())
        {
            return Err(ExportError::ScopeChanged);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("Choose one or two providers with the same history period.")]
    InvalidSelection,
    #[error("This history export is too large. Choose a shorter period.")]
    TooLarge,
    #[error("The local time zone changed during export. Please try again.")]
    TimeZoneChanged,
    #[error("The history dates or time zone changed. Refresh the chart and export again.")]
    ScopeChanged,
    #[error(transparent)]
    Query(QueryError),
}

impl From<QueryError> for ExportError {
    fn from(error: QueryError) -> Self {
        match error {
            QueryError::TooLarge => Self::TooLarge,
            other => Self::Query(other),
        }
    }
}

pub fn validate_export_requests(requests: &[ExportRequest]) -> Result<(), ExportError> {
    if requests.is_empty()
        || requests.len() > 2
        || requests.iter().any(|item| {
            item.timezone.is_empty()
                || item.timezone.len() > 128
                || item.timezone.chars().any(char::is_control)
                || item.first_date.is_some() != item.last_date.is_some()
                || item
                    .first_date
                    .as_ref()
                    .is_some_and(|date| date.len() != 10)
                || item.last_date.as_ref().is_some_and(|date| date.len() != 10)
        })
    {
        return Err(ExportError::InvalidSelection);
    }
    validate_requests(
        &requests
            .iter()
            .map(|item| item.request.clone())
            .collect::<Vec<_>>(),
    )
}

fn validate_requests(requests: &[QueryRequest]) -> Result<(), ExportError> {
    if requests.is_empty()
        || requests.len() > 2
        || requests
            .windows(2)
            .any(|pair| pair[0].provider == pair[1].provider || pair[0].range != pair[1].range)
    {
        return Err(ExportError::InvalidSelection);
    }
    Ok(())
}

pub fn csv(
    queries: &[QueryResult],
    mut check_current: impl FnMut() -> Result<(), QueryError>,
) -> Result<Vec<u8>, ExportError> {
    encode(queries, &mut check_current, MAX_BYTES)
}

fn encode(
    queries: &[QueryResult],
    check_current: &mut impl FnMut() -> Result<(), QueryError>,
    max_bytes: usize,
) -> Result<Vec<u8>, ExportError> {
    check_current()?;
    if queries
        .windows(2)
        .any(|pair| pair[0].timezone != pair[1].timezone)
    {
        return Err(ExportError::TimeZoneChanged);
    }
    if HEADER.len() > max_bytes {
        return Err(ExportError::TooLarge);
    }
    let mut output = HEADER.to_owned();
    for query in queries {
        for point in &query.points {
            check_current()?;
            let day_index = query
                .days
                .partition_point(|day| day.ends_at <= point.observed_at);
            let day = query
                .days
                .get(day_index)
                .filter(|day| day.starts_at <= point.observed_at)
                .ok_or(QueryError::InvalidReading)?;
            let provider = match query.request.provider {
                ProviderId::Claude => "claude",
                ProviderId::Codex => "codex",
            };
            let provenance = match query.request.limit.provenance {
                Provenance::Official => "official",
                Provenance::LocalEstimate => "local_estimate",
                Provenance::Unknown => "unknown",
            };
            let breaks = point
                .break_before
                .iter()
                .map(|reason| match reason {
                    BreakReason::MissingTime => "missing_time",
                    BreakReason::ResetChanged => "reset_changed",
                    BreakReason::ResetBoundary => "reset_boundary",
                    BreakReason::UsageDecreased => "usage_decreased",
                    BreakReason::LimitUnavailable => "limit_unavailable",
                })
                .collect::<Vec<_>>()
                .join(";");
            let cells = [
                timestamp(point.observed_at)?,
                query.timezone.clone(),
                day.date.clone(),
                provider.to_owned(),
                query.request.account_key.clone(),
                query.request.limit.id.clone(),
                query
                    .request
                    .limit
                    .window_seconds
                    .map_or_else(String::new, |value| value.to_string()),
                point.used_fraction.to_string(),
                (1.0 - point.used_fraction).to_string(),
                point
                    .resets_at
                    .map(timestamp)
                    .transpose()?
                    .unwrap_or_default(),
                provenance.to_owned(),
                breaks,
            ];
            let row = cells
                .iter()
                .map(|cell| quote(cell))
                .collect::<Vec<_>>()
                .join(",");
            if output.len().saturating_add(row.len()).saturating_add(2) > max_bytes {
                return Err(ExportError::TooLarge);
            }
            output.push_str(&row);
            output.push_str("\r\n");
        }
    }
    check_current()?;
    Ok(output.into_bytes())
}

fn timestamp(seconds: i64) -> Result<String, QueryError> {
    OffsetDateTime::from_unix_timestamp(seconds)
        .map_err(|_| QueryError::InvalidReading)?
        .format(&Rfc3339)
        .map_err(|_| QueryError::InvalidReading)
}

fn quote(value: &str) -> String {
    // CSV quoting alone does not stop a spreadsheet from evaluating a formula.
    let formula = value.trim_start().starts_with(['=', '+', '-', '@'])
        || value.starts_with(['\t', '\r', '\n']);
    format!(
        "\"{}{}\"",
        if formula { "'" } else { "" },
        value.replace('"', "\"\"")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        history::{HistoryLimit, HistoryRecord},
        history_query::{Accumulator, HistoryRange, LimitKey},
        platform::calendar::Calendar,
    };

    fn query() -> QueryResult {
        let observed = 1_792_887_300;
        let request = QueryRequest {
            provider: ProviderId::Claude,
            account_key: "a".repeat(64),
            limit: LimitKey {
                id: "=HYPERLINK(\"https://example.com\",\"open\")".into(),
                window_seconds: Some(18_000),
                provenance: Provenance::Official,
            },
            range: HistoryRange::AllTime {},
        };
        let mut query = Accumulator::for_export(
            request.clone(),
            Calendar::named("Europe/Warsaw").unwrap(),
            observed + 3600,
        )
        .unwrap();
        for offset in [0, 0, 3600] {
            query
                .observe(HistoryRecord {
                    version: 1,
                    provider: request.provider,
                    account_key: request.account_key.clone(),
                    observed_at: observed + offset,
                    limits: vec![HistoryLimit {
                        id: request.limit.id.clone(),
                        label: "Private account label".into(),
                        used_fraction: 0.123_456_789_012_345_66,
                        resets_at: None,
                        window_seconds: Some(18_000),
                        provenance: Provenance::Official,
                    }],
                })
                .unwrap();
        }
        query.finish().unwrap()
    }

    #[test]
    fn exports_original_deduplicated_readings_with_precision_and_safe_string_cells() {
        let query = query();
        assert_eq!(
            query.days.iter().map(|day| day.sample_count).sum::<u64>(),
            2
        );
        let bytes = csv(&[query], || Ok(())).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.starts_with(HEADER));
        assert!(text.contains("\"0.12345678901234566\",\"0.8765432109876543\""));
        assert!(text.contains("\"'=HYPERLINK(\"\"https://example.com\"\",\"\"open\"\")\""));
        assert!(text.contains("\"Europe/Warsaw\""));
        assert!(text.contains("\"2026-10-25T00:15:00Z\""));
        assert!(text.contains("\"2026-10-25T01:15:00Z\""));
        assert_eq!(text.matches("\"2026-10-25\"").count(), 2);
        assert!(text.contains("\"missing_time\""));
        assert!(!text.contains("Private account label"));
        assert!(!text.contains("access_token"));
    }

    #[test]
    fn escapes_spreadsheet_formula_prefixes_and_csv_delimiters() {
        for value in [
            "=1+1", "+1+1", "-1+1", "@SUM(A1)", "  =1+1", "\ttext", "\rtext", "\ntext",
        ] {
            assert!(quote(value).starts_with("\"'"));
        }
        assert_eq!(quote("plain, \"value\""), "\"plain, \"\"value\"\"\"");
        assert_eq!(quote("0.123456789"), "\"0.123456789\"");
    }

    #[test]
    fn oversize_cancelled_and_changed_timezone_exports_return_no_bytes() {
        let first = query();
        assert!(matches!(
            encode(
                std::slice::from_ref(&first),
                &mut || Ok(()),
                HEADER.len() + 1
            ),
            Err(ExportError::TooLarge)
        ));
        let mut checks = 0;
        assert!(matches!(
            csv(std::slice::from_ref(&first), || {
                checks += 1;
                if checks >= 3 {
                    Err(QueryError::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(ExportError::Query(QueryError::Cancelled))
        ));
        let mut second = first.clone();
        second.timezone = "UTC".into();
        assert!(matches!(
            csv(&[first, second], || Ok(())),
            Err(ExportError::TimeZoneChanged)
        ));
    }

    #[test]
    fn requires_distinct_providers_and_one_shared_period() {
        let first = query().request;
        assert!(validate_requests(&[]).is_err());
        assert!(validate_requests(&[first.clone(), first.clone()]).is_err());
        let mut other = first.clone();
        other.provider = ProviderId::Codex;
        assert!(validate_requests(&[first.clone(), other.clone()]).is_ok());
        other.range = HistoryRange::Days7 {};
        assert!(validate_requests(&[first.clone(), other]).is_err());
        assert!(validate_requests(&[first.clone(), first.clone(), first]).is_err());
    }

    #[test]
    fn refuses_a_relative_range_that_changed_while_choosing_a_destination() {
        let before_midnight = 1_790_899_199;
        let mut request = query().request;
        request.range = HistoryRange::Today {};
        let before = Accumulator::for_export(
            request.clone(),
            Calendar::named("UTC").unwrap(),
            before_midnight,
        )
        .unwrap()
        .finish()
        .unwrap();
        let selection = ExportRequest {
            request: request.clone(),
            timezone: before.timezone.clone(),
            first_date: before.days.first().map(|day| day.date.clone()),
            last_date: before.days.last().map(|day| day.date.clone()),
        };
        assert!(selection.check_scope(&before).is_ok());
        assert!(validate_export_requests(std::slice::from_ref(&selection)).is_ok());
        let after = Accumulator::for_export(
            request,
            Calendar::named("UTC").unwrap(),
            before_midnight + 1,
        )
        .unwrap()
        .finish()
        .unwrap();
        assert!(matches!(
            selection.check_scope(&after),
            Err(ExportError::ScopeChanged)
        ));
        let mut another_zone = before.clone();
        another_zone.timezone = "Europe/Warsaw".into();
        assert!(matches!(
            selection.check_scope(&another_zone),
            Err(ExportError::ScopeChanged)
        ));
    }

    #[test]
    fn refuses_all_time_when_its_first_or_last_saved_date_changes() {
        let result = query();
        let mut selection = ExportRequest {
            request: result.request.clone(),
            timezone: result.timezone.clone(),
            first_date: result.days.first().map(|day| day.date.clone()),
            last_date: result.days.last().map(|day| day.date.clone()),
        };
        assert!(selection.check_scope(&result).is_ok());
        selection.first_date = Some("2026-10-24".into());
        assert!(matches!(
            selection.check_scope(&result),
            Err(ExportError::ScopeChanged)
        ));
        selection.first_date = result.days.first().map(|day| day.date.clone());
        selection.last_date = Some("2026-10-24".into());
        assert!(matches!(
            selection.check_scope(&result),
            Err(ExportError::ScopeChanged)
        ));
    }
}
