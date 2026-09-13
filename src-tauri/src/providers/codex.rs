use crate::model::{Amount, Limit, LimitKind, Provenance, ProviderId, ProviderSnapshot};
use serde_json::Value;

pub fn parse(value: Value, fetched_at: i64) -> Result<ProviderSnapshot, String> {
    let object = value
        .as_object()
        .ok_or("Codex returned an invalid usage response.")?;
    let mut snapshot = ProviderSnapshot {
        provider: ProviderId::Codex,
        limits: Vec::new(),
        plan: object
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        fetched_at,
        warnings: Vec::new(),
    };
    for (key, label) in [
        ("rate_limit", ""),
        ("code_review_rate_limit", "Code review"),
    ] {
        if let Some(bucket) = object.get(key).filter(|value| !value.is_null()) {
            parse_bucket(bucket, key, label, &mut snapshot);
        }
    }
    if let Some(additional) = object
        .get("additional_rate_limits")
        .filter(|value| !value.is_null())
    {
        if let Some(buckets) = additional.as_array() {
            for (index, bucket) in buckets.iter().enumerate() {
                let feature = bucket
                    .get("metered_feature")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty());
                let id = feature.map_or_else(
                    || format!("additional:{index}"),
                    |feature| format!("additional:{feature}"),
                );
                let label = bucket
                    .get("limit_name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .or(feature)
                    .unwrap_or("Additional limit");
                match bucket.get("rate_limit") {
                    Some(value) if !value.is_null() => {
                        parse_bucket(value, &id, label, &mut snapshot)
                    }
                    _ => push_unknown(&id, label, &mut snapshot),
                }
                if feature.is_none() {
                    snapshot
                        .warnings
                        .push("An additional Codex limit has no stable identifier.".to_owned());
                }
            }
        } else {
            push_unknown("additional_rate_limits", "Additional limits", &mut snapshot);
        }
    }
    for (key, bucket) in object {
        if key.ends_with("_rate_limit") && key != "code_review_rate_limit" && !bucket.is_null() {
            parse_bucket(bucket, key, &key.replace('_', " "), &mut snapshot);
        }
    }
    if let Some(credits) = object.get("credits").filter(|value| !value.is_null()) {
        parse_credits(credits, &mut snapshot);
    }
    if let Some(spend) = object.get("spend_control").filter(|value| !value.is_null()) {
        parse_spend(spend, &mut snapshot);
    }
    if snapshot.limits.is_empty() {
        return Err("Codex returned no usage limits.".to_owned());
    }
    Ok(snapshot)
}

fn parse_bucket(bucket: &Value, id: &str, label: &str, snapshot: &mut ProviderSnapshot) {
    let Some(object) = bucket.as_object() else {
        push_unknown(id, label, snapshot);
        return;
    };
    let mut keys: Vec<&str> = object
        .keys()
        .filter(|key| key.ends_with("_window"))
        .map(String::as_str)
        .collect();
    keys.sort_by_key(|key| match *key {
        "primary_window" => 0,
        "secondary_window" => 1,
        _ => 2,
    });
    let mut populated = false;
    for key in keys {
        let window = &object[key];
        if window.is_null() {
            continue;
        }
        populated = true;
        let id = format!("{id}:{key}");
        if !window.is_object() {
            push_unknown(&id, label, snapshot);
            continue;
        }
        let seconds = duration(
            window.get("limit_window_seconds"),
            label,
            &mut snapshot.warnings,
        );
        let window_label = duration_label(seconds, key);
        let label = if label.is_empty() {
            window_label
        } else {
            format!("{label} · {window_label}")
        };
        let used_fraction = percent(
            window.get("used_percent"),
            false,
            &label,
            &mut snapshot.warnings,
        );
        let resets_at = reset(window.get("reset_at"), &label, &mut snapshot.warnings);
        let detail = if bucket.get("allowed").and_then(Value::as_bool) == Some(false) {
            Some("The provider currently blocks this usage bucket.".to_owned())
        } else {
            None
        };
        let mut limit = Limit {
            id,
            label,
            kind: LimitKind::Quota,
            used_fraction,
            resets_at,
            window_seconds: seconds,
            provenance: Provenance::Official,
            enabled: true,
            amount: None,
            detail,
        };
        if snapshot
            .limits
            .iter()
            .any(|existing| existing.id == limit.id)
        {
            snapshot.warnings.push(format!(
                "{} appears more than once; both readings are shown.",
                limit.label
            ));
            limit
                .id
                .push_str(&format!(":row:{}", snapshot.limits.len()));
        }
        snapshot.limits.push(limit);
    }
    for (key, value) in object {
        if !key.ends_with("_window") && (value.is_object() || value.is_array()) {
            push_unknown(&format!("{id}:{key}"), &key.replace('_', " "), snapshot);
            populated = true;
        }
    }
    if !populated {
        push_unknown(
            id,
            if label.is_empty() {
                "Usage windows"
            } else {
                label
            },
            snapshot,
        );
    }
}

fn duration(value: Option<&Value>, label: &str, warnings: &mut Vec<String>) -> Option<u64> {
    let value = value.filter(|value| !value.is_null())?;
    match value.as_u64() {
        Some(seconds) if seconds > 0 => Some(seconds),
        _ => {
            warnings.push(format!("{label} has an invalid window duration."));
            None
        }
    }
}

fn duration_label(seconds: Option<u64>, key: &str) -> String {
    match seconds {
        Some(604_800) => "Weekly".to_owned(),
        Some(seconds) if seconds % 86_400 == 0 => format!("{}-day", seconds / 86_400),
        Some(seconds) if seconds % 3600 == 0 => format!("{}-hour", seconds / 3600),
        Some(seconds) if seconds % 60 == 0 => format!("{}-minute", seconds / 60),
        Some(seconds) => format!("{seconds}-second"),
        None => key.replace('_', " "),
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
    match value.as_i64() {
        Some(timestamp) if (0..=253_402_300_799).contains(&timestamp) => Some(timestamp),
        _ => {
            warnings.push(format!("{label} has an invalid reset time."));
            None
        }
    }
}

fn parse_credits(value: &Value, snapshot: &mut ProviderSnapshot) {
    if !value.is_object() {
        push_unknown("credits", "Credits", snapshot);
        return;
    }
    let available = value.get("has_credits").and_then(Value::as_bool);
    let unlimited = value.get("unlimited").and_then(Value::as_bool);
    let balance = amount(
        value.get("balance"),
        "Credit balance",
        &mut snapshot.warnings,
    );
    let detail = if unlimited == Some(true) {
        "Unlimited credits."
    } else if available == Some(false) {
        "No credits available."
    } else if available == Some(true) {
        "Credit balance has no percentage allowance."
    } else {
        snapshot
            .warnings
            .push("Credit availability was not supplied.".to_owned());
        "Credit availability is unknown."
    };
    snapshot.limits.push(Limit {
        id: "credits".to_owned(),
        label: "Credits".to_owned(),
        kind: LimitKind::Credits,
        used_fraction: None,
        resets_at: None,
        window_seconds: None,
        provenance: Provenance::Official,
        enabled: available == Some(true) || unlimited == Some(true),
        amount: Some(Amount {
            used: None,
            limit: None,
            balance,
            currency: None,
        }),
        detail: Some(detail.to_owned()),
    });
}

fn parse_spend(value: &Value, snapshot: &mut ProviderSnapshot) {
    let Some(object) = value.as_object() else {
        push_unknown("spend_control", "Spend control", snapshot);
        return;
    };
    for (key, limit) in object {
        if !key.ends_with("_limit") || limit.is_null() {
            continue;
        }
        if key != "individual_limit" || !limit.is_object() {
            push_unknown(&format!("spend_control:{key}"), "Spend control", snapshot);
            continue;
        }
        let mut used_fraction = percent(
            limit.get("used_percent"),
            true,
            "Spend limit",
            &mut snapshot.warnings,
        );
        if used_fraction.is_none()
            && limit.get("used_percent").is_none_or(Value::is_null)
            && let Some(remaining) = limit
                .get("remaining_percent")
                .filter(|value| !value.is_null())
        {
            match remaining.as_f64() {
                Some(remaining) if remaining.is_finite() && remaining <= 100.0 => {
                    used_fraction = Some((100.0 - remaining) / 100.0);
                }
                _ => snapshot
                    .warnings
                    .push("Spend limit has an invalid remaining percentage.".to_owned()),
            }
        }
        let used = amount(limit.get("used"), "Spend used", &mut snapshot.warnings);
        let maximum = amount(limit.get("limit"), "Spend limit", &mut snapshot.warnings);
        snapshot.limits.push(Limit {
            id: "spend_control:individual_limit".to_owned(),
            label: "Spend limit".to_owned(),
            kind: LimitKind::Spend,
            used_fraction,
            resets_at: reset(limit.get("reset_at"), "Spend limit", &mut snapshot.warnings),
            window_seconds: None,
            provenance: Provenance::Official,
            enabled: true,
            amount: Some(Amount {
                used,
                limit: maximum,
                balance: None,
                currency: None,
            }),
            detail: Some(
                if object.get("reached").and_then(Value::as_bool) == Some(true) {
                    "The provider reports that the spend cap has been reached."
                } else {
                    "Provider units; no currency conversion is applied."
                }
                .to_owned(),
            ),
        });
    }
}

fn amount(value: Option<&Value>, label: &str, warnings: &mut Vec<String>) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    let parsed = value.as_str().and_then(decimal);
    if parsed.is_none() {
        warnings.push(format!("{label} has an invalid decimal amount."));
    }
    parsed
}

fn decimal(value: &str) -> Option<String> {
    let (whole, fractional) = value
        .split_once('.')
        .map_or((value, None), |(whole, fractional)| {
            (whole, Some(fractional))
        });
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if fractional.is_some_and(|fractional| {
        fractional.is_empty() || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    Some(match fractional {
        Some(fractional) => format!("{whole}.{fractional}"),
        None => whole.to_owned(),
    })
}

fn push_unknown(id: &str, label: &str, snapshot: &mut ProviderSnapshot) {
    snapshot.warnings.push(format!(
        "{label} has an unrecognized shape and is excluded from the menu bar."
    ));
    snapshot.limits.push(Limit {
        id: id.to_owned(),
        label: label.to_owned(),
        kind: LimitKind::Unknown,
        used_fraction: None,
        resets_at: None,
        window_seconds: None,
        provenance: Provenance::Unknown,
        enabled: false,
        amount: None,
        detail: Some("The provider returned a field Delta-V cannot interpret yet.".to_owned()),
    });
}

#[cfg(test)]
mod tests {
    use super::parse;
    use serde_json::{Value, json};

    #[test]
    fn captured_response_matches_the_normalization_contract() {
        let captured: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/codex-wham-usage.json"))
                .unwrap();
        let actual = serde_json::to_value(parse(captured, 1_789_326_413).unwrap()).unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/codex-normalized.json"))
                .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn all_reported_windows_survive_matching_values_and_arbitrary_durations() {
        let result = parse(json!({
            "rate_limit":{
                "primary_window":{"used_percent":20,"limit_window_seconds":900,"reset_at":123},
                "secondary_window":{"used_percent":20,"limit_window_seconds":86400,"reset_at":123},
                "tertiary_window":{"used_percent":20,"limit_window_seconds":1800,"reset_at":123}
            },
            "code_review_rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":604800,"reset_at":123}},
            "additional_rate_limits":[{"metered_feature":"other","limit_name":"Other","rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":3600,"reset_at":123}}}]
        }), 0).unwrap();
        assert_eq!(result.limits.len(), 5);
        assert_eq!(result.limits[0].label, "15-minute");
        assert_eq!(result.limits[2].window_seconds, Some(1800));
        assert!(
            result
                .limits
                .iter()
                .all(|limit| limit.used_fraction == Some(0.2))
        );
    }

    #[test]
    fn missing_and_invalid_fields_are_unavailable_even_when_bucket_is_blocked() {
        let result = parse(json!({"rate_limit":{
            "allowed":false,"limit_reached":true,
            "primary_window":{"used_percent":null,"reset_after_seconds":60},
            "secondary_window":{"used_percent":-5,"reset_at":1789326413000_i64,"limit_window_seconds":0}
        }}), 42).unwrap();
        assert!(
            result
                .limits
                .iter()
                .all(|limit| limit.used_fraction.is_none() && limit.resets_at.is_none())
        );
        assert_eq!(result.warnings.len(), 3);
        assert!(result.limits[0].detail.is_some());
    }

    #[test]
    fn credits_and_spend_preserve_decimal_precision_without_a_currency_guess() {
        let result = parse(json!({
            "credits":{"has_credits":true,"unlimited":false,"balance":"9007199254740993.05"},
            "spend_control":{"reached":false,"individual_limit":{"used":"8000.01","limit":"25000","remaining_percent":68,"reset_at":1789326413}},
            "email":"discard@example.invalid","account_id":"discard-account","user_id":"discard-user"
        }), 0).unwrap();
        assert!(result.limits[0].used_fraction.is_none());
        assert_eq!(
            result.limits[0].amount.as_ref().unwrap().balance.as_deref(),
            Some("9007199254740993.05")
        );
        assert_eq!(result.limits[1].used_fraction, Some(0.32));
        assert!(result.limits[1].amount.as_ref().unwrap().currency.is_none());
        assert!(!serde_json::to_string(&result).unwrap().contains("discard"));
    }

    #[test]
    fn unknown_buckets_remain_visible_without_inventing_usage() {
        let result = parse(json!({"rate_limit":{"future_allowance":{"used":12}},"credits":{"unlimited":true,"has_credits":true,"balance":null}}), 0).unwrap();
        assert_eq!(result.limits.len(), 2);
        assert!(
            result
                .limits
                .iter()
                .all(|limit| limit.used_fraction.is_none())
        );
        assert_eq!(result.warnings.len(), 1);
        assert!(!result.limits[0].enabled);
        assert!(result.limits[1].enabled);
    }
}
