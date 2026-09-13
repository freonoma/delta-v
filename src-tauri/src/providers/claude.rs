use crate::model::{Amount, Limit, LimitKind, Provenance, ProviderId, ProviderSnapshot};
use serde_json::Value;
use std::collections::HashSet;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn parse(value: Value, fetched_at: i64) -> Result<ProviderSnapshot, String> {
    let object = value
        .as_object()
        .ok_or("Claude returned an invalid usage response.")?;
    let mut snapshot = ProviderSnapshot {
        provider: ProviderId::Claude,
        limits: Vec::new(),
        plan: None,
        fetched_at,
        warnings: Vec::new(),
    };
    let mut aliases = HashSet::new();
    if let Some(rows) = object.get("limits").filter(|value| !value.is_null()) {
        if let Some(rows) = rows.as_array() {
            for (index, row) in rows.iter().enumerate() {
                let kind = row.get("kind").and_then(Value::as_str);
                let (id, label, seconds) = match kind {
                    Some("session") => {
                        aliases.insert("five_hour".to_owned());
                        ("session".to_owned(), "5-hour".to_owned(), 18_000)
                    }
                    Some("weekly_all") => {
                        aliases.insert("seven_day".to_owned());
                        ("weekly".to_owned(), "Weekly".to_owned(), 604_800)
                    }
                    Some("weekly_scoped") => scoped_identity(row, index, &mut aliases),
                    _ => {
                        let name = kind.unwrap_or("unrecognized");
                        let limit = unknown(
                            format!("limits:{index}"),
                            format!("Unrecognized limit ({name})"),
                            &mut snapshot.warnings,
                        );
                        snapshot.limits.push(limit);
                        continue;
                    }
                };
                let mut limit = quota(id, label, seconds, row, "percent", &mut snapshot.warnings);
                if kind == Some("weekly_scoped")
                    && row
                        .pointer("/scope/model")
                        .is_some_and(|value| !value.is_null())
                    && row
                        .pointer("/scope/model/id")
                        .and_then(Value::as_str)
                        .is_none()
                {
                    limit.detail = Some("This scope has no stable model identifier.".to_owned());
                }
                push_unique(&mut snapshot, limit, index);
            }
        } else {
            snapshot
                .warnings
                .push("Claude's limits list has an unrecognized shape.".to_owned());
        }
    }

    for (key, row) in object {
        if row.is_null() || aliases.contains(key) {
            continue;
        }
        let (id, label, seconds) = match key.as_str() {
            "five_hour" => ("session".to_owned(), "5-hour".to_owned(), 18_000),
            "seven_day" => ("weekly".to_owned(), "Weekly".to_owned(), 604_800),
            "limits"
            | "extra_usage"
            | "spend"
            | "seven_day_breakdown"
            | "member_dashboard_available" => continue,
            key if key.starts_with("seven_day_") => (
                format!("weekly:legacy:{key}"),
                format!("Weekly · {}", title(&key[10..])),
                604_800,
            ),
            _ if row.is_object() || row.is_array() => {
                let limit = unknown(key.clone(), key.clone(), &mut snapshot.warnings);
                snapshot.limits.push(limit);
                continue;
            }
            _ => continue,
        };
        snapshot.limits.push(quota(
            id,
            label,
            seconds,
            row,
            "utilization",
            &mut snapshot.warnings,
        ));
    }

    if let Some(spend) = object.get("spend").filter(|value| !value.is_null()) {
        snapshot
            .limits
            .push(spend_limit(spend, &mut snapshot.warnings));
    } else if let Some(extra) = object.get("extra_usage").filter(|value| !value.is_null()) {
        snapshot
            .limits
            .push(legacy_spend(extra, &mut snapshot.warnings));
    }
    if let Some(extra) = object.get("extra_usage") {
        for key in ["daily", "weekly"] {
            if extra.get(key).is_some_and(|value| !value.is_null()) {
                let limit = unknown(
                    format!("extra_usage:{key}"),
                    format!("Extra usage · {key}"),
                    &mut snapshot.warnings,
                );
                snapshot.limits.push(limit);
            }
        }
    }
    if snapshot.limits.is_empty() {
        return Err("Claude returned no usage limits.".to_owned());
    }
    snapshot
        .limits
        .sort_by_key(|limit| match limit.id.as_str() {
            "session" => 0,
            "weekly" => 1,
            _ if limit.kind == LimitKind::Quota => 2,
            _ if limit.kind != LimitKind::Unknown => 3,
            _ => 4,
        });
    Ok(snapshot)
}

fn scoped_identity(
    row: &Value,
    index: usize,
    aliases: &mut HashSet<String>,
) -> (String, String, u64) {
    let model_id = row
        .pointer("/scope/model/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let model_name = row
        .pointer("/scope/model/display_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty());
    let surface = row.pointer("/scope/surface");
    let surface_id = surface.and_then(|value| {
        value
            .as_str()
            .or_else(|| value.get("id").and_then(Value::as_str))
    });
    let surface_name = surface
        .and_then(|value| value.get("display_name").and_then(Value::as_str))
        .or(surface_id);
    let model_key = match (model_id, model_name) {
        (Some(id), _) => format!("model:{id}"),
        (None, Some(name)) => format!("model-name:{name}"),
        _ if surface_id.is_some() => "scope".to_owned(),
        _ => format!("row:{index}"),
    };
    // A display name alone does not establish equivalence with a legacy model key.
    if surface.is_none_or(Value::is_null)
        && let Some(id @ ("opus" | "sonnet")) = model_id
    {
        aliases.insert(format!("seven_day_{id}"));
    }
    let mut id = format!("weekly:{model_key}");
    let mut names = vec![];
    if let Some(name) = model_name.or(model_id) {
        names.push(name);
    }
    if let Some(surface) = surface_id {
        id.push_str(&format!(":surface:{surface}"));
    }
    if let Some(name) = surface_name {
        names.push(name);
    }
    let label = if names.is_empty() {
        format!("Weekly · scope {}", index + 1)
    } else {
        format!("Weekly · {}", names.join(" · "))
    };
    (id, label, 604_800)
}

fn push_unique(snapshot: &mut ProviderSnapshot, mut limit: Limit, index: usize) {
    if snapshot
        .limits
        .iter()
        .any(|existing| existing.id == limit.id)
    {
        snapshot.warnings.push(format!(
            "{} appears more than once; both readings are shown.",
            limit.label
        ));
        limit.id.push_str(&format!(":row:{index}"));
    }
    snapshot.limits.push(limit);
}

fn quota(
    id: String,
    label: String,
    seconds: u64,
    row: &Value,
    percent_key: &str,
    warnings: &mut Vec<String>,
) -> Limit {
    if !row.is_object() {
        return unknown(id, label, warnings);
    }
    Limit {
        id,
        used_fraction: percent(row.get(percent_key), false, &label, warnings),
        resets_at: reset(row.get("resets_at"), &label, warnings),
        label,
        kind: LimitKind::Quota,
        window_seconds: Some(seconds),
        provenance: Provenance::Official,
        enabled: true,
        amount: None,
        detail: None,
    }
}

fn unknown(id: String, label: String, warnings: &mut Vec<String>) -> Limit {
    warnings.push(format!(
        "{label} is not yet understood and is excluded from the menu bar."
    ));
    Limit {
        id,
        label,
        kind: LimitKind::Unknown,
        used_fraction: None,
        resets_at: None,
        window_seconds: None,
        provenance: Provenance::Unknown,
        enabled: false,
        amount: None,
        detail: Some("The provider returned a field Delta-V cannot interpret yet.".to_owned()),
    }
}

fn percent(
    value: Option<&Value>,
    monetary: bool,
    label: &str,
    warnings: &mut Vec<String>,
) -> Option<f64> {
    let value = value.filter(|value| !value.is_null())?;
    match value.as_f64() {
        Some(number) if number.is_finite() && number >= 0.0 && (monetary || number <= 100.0) => {
            Some(number / 100.0)
        }
        _ => {
            warnings.push(format!(
                "{label} has an invalid percentage; usage is unavailable."
            ));
            None
        }
    }
}

fn reset(value: Option<&Value>, label: &str, warnings: &mut Vec<String>) -> Option<i64> {
    let value = value.filter(|value| !value.is_null())?;
    let parsed = value
        .as_str()
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok());
    match parsed {
        Some(timestamp) => Some(timestamp.unix_timestamp()),
        None => {
            warnings.push(format!("{label} has an invalid reset time."));
            None
        }
    }
}

fn spend_limit(row: &Value, warnings: &mut Vec<String>) -> Limit {
    if !row.is_object() {
        return unknown("extra_usage".to_owned(), "Extra usage".to_owned(), warnings);
    }
    let mut currency = None;
    let mut amount = Amount {
        used: None,
        limit: None,
        balance: None,
        currency: None,
    };
    let mut mismatched = false;
    for (key, target) in [
        ("used", &mut amount.used),
        ("limit", &mut amount.limit),
        ("balance", &mut amount.balance),
    ] {
        if let Some(money) = row.get(key).filter(|value| !value.is_null()) {
            match money_value(money) {
                Some((value, unit))
                    if currency.as_ref().is_none_or(|existing| existing == &unit) =>
                {
                    currency = Some(unit);
                    *target = Some(value);
                }
                Some(_) => mismatched = true,
                None => warnings.push(format!("Extra usage {key} has unknown monetary units.")),
            }
        }
    }
    amount.currency = currency;
    let enabled = row.get("enabled").and_then(Value::as_bool);
    if enabled.is_none() {
        warnings.push("Extra usage availability was not supplied.".to_owned());
    }
    if mismatched {
        warnings.push(
            "Extra usage amounts use different currencies and cannot be combined.".to_owned(),
        );
    }
    let used_fraction = if enabled == Some(true) {
        percent(row.get("percent"), true, "Extra usage", warnings)
    } else {
        None
    };
    Limit {
        id: "extra_usage".to_owned(),
        label: "Extra usage".to_owned(),
        kind: LimitKind::Spend,
        used_fraction,
        resets_at: reset(row.get("resets_at"), "Extra usage", warnings),
        window_seconds: None,
        provenance: Provenance::Official,
        enabled: enabled == Some(true),
        amount: if mismatched { None } else { Some(amount) },
        detail: match enabled {
            Some(false) => Some("Extra usage is turned off.".to_owned()),
            None => Some("Extra usage availability is unknown.".to_owned()),
            _ => None,
        },
    }
}

fn legacy_spend(row: &Value, warnings: &mut Vec<String>) -> Limit {
    if !row.is_object() {
        return unknown("extra_usage".to_owned(), "Extra usage".to_owned(), warnings);
    }
    let enabled = row.get("is_enabled").and_then(Value::as_bool);
    let currency = row
        .get("currency")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let places = row.get("decimal_places").and_then(Value::as_u64);
    let mut amount = Amount {
        used: None,
        limit: None,
        balance: None,
        currency: currency.map(str::to_owned),
    };
    for (key, target) in [
        ("used_credits", &mut amount.used),
        ("monthly_limit", &mut amount.limit),
    ] {
        if let Some(value) = row.get(key).filter(|value| !value.is_null()) {
            *target = currency
                .and(places)
                .and_then(|places| minor_decimal(value, places));
            if target.is_none() {
                warnings.push(format!("Extra usage {key} has unknown monetary units."));
            }
        }
    }
    let used_fraction = if enabled == Some(true) {
        percent(row.get("utilization"), true, "Extra usage", warnings)
    } else {
        None
    };
    if enabled.is_none() {
        warnings.push("Extra usage availability was not supplied.".to_owned());
    }
    Limit {
        id: "extra_usage".to_owned(),
        label: "Extra usage".to_owned(),
        kind: LimitKind::Spend,
        used_fraction,
        resets_at: reset(row.get("resets_at"), "Extra usage", warnings),
        window_seconds: None,
        provenance: Provenance::Official,
        enabled: enabled == Some(true),
        amount: Some(amount),
        detail: match enabled {
            Some(false) => Some("Extra usage is turned off.".to_owned()),
            None => Some("Extra usage availability is unknown.".to_owned()),
            _ => None,
        },
    }
}

fn money_value(value: &Value) -> Option<(String, String)> {
    let currency = value
        .get("currency")?
        .as_str()
        .filter(|currency| !currency.is_empty())?;
    let places = value.get("exponent")?.as_u64()?;
    let amount = minor_decimal(value.get("amount_minor")?, places)?;
    Some((amount, currency.to_owned()))
}

fn minor_decimal(value: &Value, places: u64) -> Option<String> {
    if places > 18 {
        return None;
    }
    let raw = match value {
        Value::Number(value) if value.is_u64() => value.to_string(),
        Value::String(value) => value.clone(),
        _ => return None,
    };
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits = raw.trim_start_matches('0');
    let mut digits = if digits.is_empty() {
        "0".to_owned()
    } else {
        digits.to_owned()
    };
    let places = usize::try_from(places).ok()?;
    if places > 0 {
        if digits.len() <= places {
            digits = format!("{}{}", "0".repeat(places + 1 - digits.len()), digits);
        }
        digits.insert(digits.len() - places, '.');
    }
    Some(digits)
}

fn title(value: &str) -> String {
    value
        .split('_')
        .map(|word| {
            let mut letters = word.chars();
            match letters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + letters.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::{Value, json};

    #[test]
    fn captured_response_matches_the_normalization_contract() {
        let captured: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/claude-oauth-usage.json"))
                .unwrap();
        let actual = serde_json::to_value(parse(captured, 1_789_326_958).unwrap()).unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/claude-normalized.json"))
                .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn scoped_limits_keep_identity_even_when_values_match_and_activity_is_false() {
        let result = parse(json!({
            "limits": [
                {"kind":"weekly_scoped","percent":25,"is_active":false,"scope":{"model":{"id":null,"display_name":"Fable"}}},
                {"kind":"weekly_scoped","percent":25,"is_active":false,"scope":{"model":{"id":"another-model","display_name":"Another"}}}
            ],
            "seven_day_fable":{"utilization":25,"resets_at":null}
        }), 0).unwrap();
        assert_eq!(result.limits.len(), 3);
        assert!(
            result
                .limits
                .iter()
                .all(|limit| limit.enabled && limit.used_fraction == Some(0.25))
        );
        assert_ne!(result.limits[0].id, result.limits[1].id);
        assert_ne!(result.limits[0].id, result.limits[2].id);
    }

    #[test]
    fn malformed_and_missing_values_do_not_become_zero() {
        let result = parse(json!({
            "limits":[{"kind":"session","percent":101,"resets_at":"invalid"},{"kind":"weekly_all","percent":null}],
            "five_hour":{"utilization":45},
            "future_pool":{"utilization":0,"resets_at":null}
        }), 0).unwrap();
        assert_eq!(result.limits.len(), 3);
        assert!(
            result
                .limits
                .iter()
                .all(|limit| limit.used_fraction.is_none() && limit.resets_at.is_none())
        );
        assert_eq!(result.warnings.len(), 3);
    }

    #[test]
    fn monetary_precision_and_units_are_preserved_without_a_cents_assumption() {
        let result = parse(json!({"extra_usage":{
            "is_enabled":true,"used_credits":"9007199254740993","monthly_limit":"10000000000000000",
            "currency":"JPY","decimal_places":0,"utilization":120
        }}), 0).unwrap();
        let amount = result.limits[0].amount.as_ref().unwrap();
        assert_eq!(amount.used.as_deref(), Some("9007199254740993"));
        assert_eq!(result.limits[0].used_fraction, Some(1.2));
        let missing_units = parse(
            json!({"extra_usage":{"is_enabled":true,"used_credits":123,"monthly_limit":500}}),
            0,
        )
        .unwrap();
        assert!(
            missing_units.limits[0]
                .amount
                .as_ref()
                .unwrap()
                .used
                .is_none()
        );
        assert!(missing_units.limits[0].used_fraction.is_none());
    }

    #[test]
    fn reset_offsets_cross_dst_as_utc_instants() {
        let result = parse(
            json!({
                "five_hour":{"utilization":1,"resets_at":"2026-10-25T02:30:00+02:00"},
                "seven_day":{"utilization":1,"resets_at":"2026-10-25T02:30:00+01:00"}
            }),
            0,
        )
        .unwrap();
        assert_eq!(
            result.limits[1].resets_at.unwrap() - result.limits[0].resets_at.unwrap(),
            3600
        );
    }
}
