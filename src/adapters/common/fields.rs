//! Reading the fields harness records carry: epoch numbers as ISO-8601
//! moments, non-empty text, integers, and the text a content or tool value
//! stands for.

use serde_json::Value;

/// ISO-8601 text of a moment given in milliseconds since the epoch.
pub(crate) fn iso_millis(millis: i64) -> Option<String> {
    let stamp = chrono::DateTime::from_timestamp_millis(millis)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// `new Date(ms).toISOString()` for a moment a harness wrote as a JSON
/// number of milliseconds: a value that is not finite, or outside the
/// representable range, threw there and is no moment here.
pub(crate) fn iso_epoch_millis(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    iso_millis(millis as i64)
}

/// ISO-8601 text of a moment given in (possibly fractional) seconds since the
/// epoch; a negative or non-finite value is no moment.
pub(crate) fn iso_seconds(seconds: f64) -> Option<String> {
    let Ok(since) = std::time::Duration::try_from_secs_f64(seconds) else {
        return None;
    };
    let stamp: chrono::DateTime<chrono::Utc> = std::time::UNIX_EPOCH.checked_add(since)?.into();
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// A JSON number field holding epoch milliseconds, as ISO-8601 text.
pub(crate) fn iso_millis_field(value: &Value, key: &str) -> Option<String> {
    iso_millis(value.get(key)?.as_f64()? as i64)
}

/// A JSON string field, when present and non-empty.
pub(crate) fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// A JSON integer field: an integer as written, or a finite number truncated
/// to one, as `typeof x === 'number'` accepted it.
pub(crate) fn int_field(value: &Value, key: &str) -> Option<i64> {
    let number = value.get(key)?;
    number.as_i64().or_else(|| {
        number
            .as_f64()
            .filter(|raw| raw.is_finite())
            .map(|raw| raw as i64)
    })
}

/// The words a content value carries: a string as is, an array of strings or
/// of blocks with a `text` field joined by newlines.
pub(crate) fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Object(map) => map.get("text").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A tool input or output as the text an event carries: a string as is,
/// anything else as compact JSON.
pub(crate) fn as_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}
