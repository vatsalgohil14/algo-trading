//! Market quote snapshots: LTP, OHLC and full quote with depth, for up to
//! 1000 instruments per call (1 call per second).

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDateTime;
use reqwest::Method;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::DhanClient;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{DhanError, ExchangeSegment, InstrumentId, Result, SecurityId};

pub const MAX_QUOTE_INSTRUMENTS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LtpQuote {
    #[serde(default, deserialize_with = "su::decimal")]
    pub last_price: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Ohlc {
    #[serde(default, deserialize_with = "su::decimal")]
    pub open: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub high: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub low: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub close: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OhlcQuote {
    #[serde(default, deserialize_with = "su::decimal")]
    pub last_price: Decimal,
    #[serde(default)]
    pub ohlc: Ohlc,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct DepthEntry {
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub orders: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub price: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Depth {
    #[serde(default)]
    pub buy: Vec<DepthEntry>,
    #[serde(default)]
    pub sell: Vec<DepthEntry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FullQuote {
    /// Day VWAP.
    #[serde(default, deserialize_with = "su::decimal")]
    pub average_price: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub buy_quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub sell_quantity: i64,
    #[serde(default)]
    pub depth: Depth,
    #[serde(default, deserialize_with = "su::decimal")]
    pub last_price: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub last_quantity: i64,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub last_trade_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::decimal")]
    pub lower_circuit_limit: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub upper_circuit_limit: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub net_change: Decimal,
    #[serde(default)]
    pub ohlc: Ohlc,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub oi: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub oi_day_high: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub oi_day_low: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub volume: i64,
}

/// Builds `{"NSE_EQ": [11536, ...], ...}`.
fn request_body(instruments: &[InstrumentId]) -> Result<Value> {
    if instruments.is_empty() {
        return Err(DhanError::InvalidRequest("no instruments given".into()));
    }
    if instruments.len() > MAX_QUOTE_INSTRUMENTS {
        return Err(DhanError::InvalidRequest(format!(
            "at most {MAX_QUOTE_INSTRUMENTS} instruments per quote request, got {}",
            instruments.len()
        )));
    }
    let mut grouped: BTreeMap<ExchangeSegment, Vec<u32>> = BTreeMap::new();
    for i in instruments {
        grouped.entry(i.segment).or_default().push(i.security_id.0);
    }
    let map = grouped
        .into_iter()
        .map(|(seg, ids)| (seg.as_str().to_owned(), Value::from(ids)))
        .collect();
    Ok(Value::Object(map))
}

/// Turns `{"data": {"NSE_EQ": {"11536": {...}}}}` into a flat map. Entries
/// that cannot be parsed are skipped with a warning rather than failing the
/// whole snapshot.
fn parse_response<T: DeserializeOwned>(
    endpoint: &str,
    v: Value,
) -> Result<HashMap<InstrumentId, T>> {
    let data = match v {
        Value::Object(mut m) => m.remove("data").unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let Value::Object(segments) = data else {
        return Err(DhanError::decode(endpoint, "missing `data` object", b""));
    };
    let mut out = HashMap::new();
    for (seg_name, by_id) in segments {
        let Ok(segment) = seg_name.parse::<ExchangeSegment>() else {
            tracing::warn!(segment = %seg_name, "unknown segment in quote response");
            continue;
        };
        let Value::Object(by_id) = by_id else {
            continue;
        };
        for (id, quote) in by_id {
            let (Ok(security_id), Ok(q)) =
                (id.parse::<SecurityId>(), serde_json::from_value::<T>(quote))
            else {
                tracing::warn!(%segment, security_id = %id, "unparseable quote entry");
                continue;
            };
            out.insert(
                InstrumentId {
                    segment,
                    security_id,
                },
                q,
            );
        }
    }
    Ok(out)
}

impl DhanClient {
    async fn quote_call<T: DeserializeOwned>(
        &self,
        path: &str,
        instruments: &[InstrumentId],
    ) -> Result<HashMap<InstrumentId, T>> {
        let body = request_body(instruments)?;
        let v: Value = self
            .send_json(Method::POST, path, &body, false, ApiCategory::Quote)
            .await?;
        parse_response(path, v)
    }

    pub async fn ltp(
        &self,
        instruments: &[InstrumentId],
    ) -> Result<HashMap<InstrumentId, LtpQuote>> {
        self.quote_call("/marketfeed/ltp", instruments).await
    }

    pub async fn ohlc(
        &self,
        instruments: &[InstrumentId],
    ) -> Result<HashMap<InstrumentId, OhlcQuote>> {
        self.quote_call("/marketfeed/ohlc", instruments).await
    }

    /// Full quote: depth, OHLC, circuit limits, volume, OI.
    pub async fn quote(
        &self,
        instruments: &[InstrumentId],
    ) -> Result<HashMap<InstrumentId, FullQuote>> {
        self.quote_call("/marketfeed/quote", instruments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_grouped_request() -> Result<()> {
        let body = request_body(&[
            InstrumentId::nse_eq(11536),
            InstrumentId::new(ExchangeSegment::NseFno, 49081),
            InstrumentId::nse_eq(1333),
        ])?;
        assert_eq!(body, json!({"NSE_EQ": [11536, 1333], "NSE_FNO": [49081]}));
        assert!(request_body(&[]).is_err());
        Ok(())
    }

    #[test]
    fn parses_nested_response() -> Result<()> {
        let v = json!({
            "data": {
                "NSE_EQ": {"11536": {"last_price": 4520}},
                "NSE_FNO": {"49081": {"last_price": 368.15}, "bad": {"last_price": 1}}
            },
            "status": "success"
        });
        let m: HashMap<InstrumentId, LtpQuote> = parse_response("/marketfeed/ltp", v)?;
        assert_eq!(m.len(), 2);
        assert_eq!(
            m.get(&InstrumentId::new(ExchangeSegment::NseFno, 49081))
                .map(|q| q.last_price),
            Some(Decimal::new(36815, 2))
        );
        Ok(())
    }
}
