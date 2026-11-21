//! Lenient serde helpers for Dhan responses.
//!
//! Dhan is not consistent about JSON types: numbers sometimes arrive as
//! strings, and `""`, `"NA"` or `null` stand in for missing values. A single
//! odd field must never make a whole order book unreadable, so response
//! fields go through these helpers instead of the derive defaults.

use std::str::FromStr;

use chrono::NaiveDateTime;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::de::{DeserializeOwned, Error as _};
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serializer};
use serde_json::Value;

fn is_blank(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || t.eq_ignore_ascii_case("NA") || t.eq_ignore_ascii_case("null")
}

fn parse_decimal_str(s: &str) -> Result<Decimal, String> {
    let t = s.trim();
    Decimal::from_str(t)
        .or_else(|_| Decimal::from_scientific(t))
        .map_err(|e| format!("invalid decimal {t:?}: {e}"))
}

pub(crate) fn decimal_from_value(v: &Value) -> Result<Option<Decimal>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => parse_decimal_str(&n.to_string()).map(Some),
        Value::String(s) if is_blank(s) => Ok(None),
        Value::String(s) => parse_decimal_str(s).map(Some),
        other => Err(format!("expected a number, got {other}")),
    }
}

fn i64_from_value(v: &Value) -> Result<Option<i64>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok()))
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            .map(Some)
            .ok_or_else(|| format!("expected an integer, got {n}")),
        Value::String(s) if is_blank(s) => Ok(None),
        Value::String(s) => {
            let t = s.trim();
            t.parse::<i64>()
                .ok()
                .or_else(|| {
                    parse_decimal_str(t)
                        .ok()
                        .filter(|d| d.fract().is_zero())
                        .and_then(|d| d.to_i64())
                })
                .map(Some)
                .ok_or_else(|| format!("expected an integer, got {s:?}"))
        }
        other => Err(format!("expected an integer, got {other}")),
    }
}

/// Number, numeric string, or blank (→ 0).
pub(crate) fn decimal<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
    let v = Value::deserialize(d)?;
    decimal_from_value(&v)
        .map(Option::unwrap_or_default)
        .map_err(D::Error::custom)
}

/// Number, numeric string, or blank (→ `None`).
pub(crate) fn opt_decimal<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Decimal>, D::Error> {
    let v = Value::deserialize(d)?;
    decimal_from_value(&v).map_err(D::Error::custom)
}

/// Integer, integer string, or blank (→ 0).
pub(crate) fn i64_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = Value::deserialize(d)?;
    i64_from_value(&v)
        .map(Option::unwrap_or_default)
        .map_err(D::Error::custom)
}

/// String, number (stringified), or blank (→ `None`).
pub(crate) fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) if is_blank(&s) => None,
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

/// Like [`opt_string`] but blank becomes `""`.
pub(crate) fn string_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    opt_string(d).map(Option::unwrap_or_default)
}

/// `true`/`false`, `"true"`/`"Y"`/`"1"` and friends; blank → `false`.
pub(crate) fn bool_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().is_some_and(|i| i != 0),
        Value::String(s) => matches!(
            s.trim().to_ascii_uppercase().as_str(),
            "TRUE" | "Y" | "YES" | "1" | "ACTIVE"
        ),
        _ => false,
    })
}

const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M:%S%.f",
    "%d/%m/%Y %H:%M:%S",
];

pub(crate) fn parse_datetime(s: &str) -> Option<NaiveDateTime> {
    let t = s.trim();
    if is_blank(t) || t.starts_with("0001-01-01") || t.starts_with("01/01/1980") {
        return None;
    }
    DATETIME_FORMATS
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(t, f).ok())
}

/// Dhan timestamps are IST wall-clock strings. Unparseable or placeholder
/// values become `None`; the raw string is not needed for trading decisions.
pub(crate) fn opt_datetime<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<NaiveDateTime>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => parse_datetime(&s),
        _ => None,
    })
}

/// Deserializes an enum leniently: unknown or blank values become `Default`.
pub(crate) fn enum_lenient<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

pub(crate) fn opt_enum_lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Null => None,
        Value::String(ref s) if is_blank(s) => None,
        other => serde_json::from_value(other).ok(),
    })
}

/// Serializes a decimal as a JSON number, which is what Dhan expects.
pub(crate) fn ser_decimal<S: Serializer>(d: &Decimal, s: S) -> Result<S::Ok, S::Error> {
    let f = d
        .to_f64()
        .ok_or_else(|| S::Error::custom(format!("decimal {d} does not fit in f64")))?;
    s.serialize_f64(f)
}

pub(crate) fn ser_opt_decimal<S: Serializer>(d: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
    match d {
        Some(d) => ser_decimal(d, s),
        None => s.serialize_none(),
    }
}

/// Accepts a JSON array, a single object, or null/empty and returns a list.
/// Several Dhan endpoints document one shape and return the other.
pub(crate) fn one_or_many<T: DeserializeOwned>(v: Value) -> Result<Vec<T>, serde_json::Error> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items.into_iter().map(serde_json::from_value).collect(),
        Value::Object(ref m) if m.is_empty() => Ok(Vec::new()),
        other => Ok(vec![serde_json::from_value(other)?]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Row {
        #[serde(deserialize_with = "decimal")]
        price: Decimal,
        #[serde(deserialize_with = "i64_lenient")]
        qty: i64,
        #[serde(deserialize_with = "opt_string")]
        id: Option<String>,
    }

    #[test]
    fn accepts_mixed_types() -> Result<(), serde_json::Error> {
        let a: Row = serde_json::from_str(r#"{"price": 3345.8, "qty": "40", "id": 12}"#)?;
        assert_eq!(a.price, Decimal::new(33458, 1));
        assert_eq!(a.qty, 40);
        assert_eq!(a.id.as_deref(), Some("12"));

        let b: Row = serde_json::from_str(r#"{"price": "NA", "qty": "", "id": ""}"#)?;
        assert_eq!(b.price, Decimal::ZERO);
        assert_eq!(b.qty, 0);
        assert_eq!(b.id, None);
        Ok(())
    }

    #[test]
    fn float_round_trip_is_exact() -> Result<(), String> {
        let d = decimal_from_value(&serde_json::json!(1428.05))?;
        assert_eq!(d, Some(Decimal::new(142805, 2)));
        Ok(())
    }

    #[test]
    fn parses_dhan_datetimes() {
        assert!(parse_datetime("2021-11-24 13:33:03").is_some());
        assert!(parse_datetime("2025-09-23T12:37:23").is_some());
        assert!(parse_datetime("0001-01-01 00:00:00").is_none());
        assert!(parse_datetime("NA").is_none());
    }
}
