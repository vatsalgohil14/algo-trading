//! Historical OHLCV: daily candles (since listing) and intraday candles
//! (1/5/15/25/60 minute, last 5 years, at most 90 days per call).

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeDelta};
use reqwest::Method;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use super::DhanClient;
use crate::rate_limit::ApiCategory;
use crate::serde_util::decimal_from_value;
use crate::{DhanError, ExchangeSegment, InstrumentKind, Result, SecurityId};

/// Longest range Dhan serves in one intraday request.
pub const MAX_INTRADAY_DAYS: i64 = 90;

const IST_OFFSET_SECS: i32 = 5 * 3600 + 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandleInterval {
    Min1,
    Min5,
    Min15,
    Min25,
    Min60,
}

impl CandleInterval {
    pub const fn minutes(self) -> u32 {
        match self {
            Self::Min1 => 1,
            Self::Min5 => 5,
            Self::Min15 => 15,
            Self::Min25 => 25,
            Self::Min60 => 60,
        }
    }
}

impl Serialize for CandleInterval {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(self.minutes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candle {
    /// Candle start, epoch seconds (UTC).
    pub timestamp: i64,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: i64,
    /// Present only when requested (F&O).
    pub open_interest: Option<i64>,
}

impl Candle {
    /// Candle start in Indian Standard Time.
    pub fn time_ist(&self) -> Option<DateTime<FixedOffset>> {
        let ist = FixedOffset::east_opt(IST_OFFSET_SECS)?;
        DateTime::from_timestamp(self.timestamp, 0).map(|t| t.with_timezone(&ist))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyCandlesRequest {
    pub security_id: SecurityId,
    pub exchange_segment: ExchangeSegment,
    pub instrument: InstrumentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry_code: Option<u8>,
    pub oi: bool,
    #[serde(serialize_with = "ser_date")]
    pub from_date: NaiveDate,
    /// Exclusive.
    #[serde(serialize_with = "ser_date")]
    pub to_date: NaiveDate,
}

impl DailyCandlesRequest {
    pub fn equity(
        segment: ExchangeSegment,
        security_id: SecurityId,
        from: NaiveDate,
        to_exclusive: NaiveDate,
    ) -> Self {
        Self {
            security_id,
            exchange_segment: segment,
            instrument: InstrumentKind::Equity,
            expiry_code: None,
            oi: false,
            from_date: from,
            to_date: to_exclusive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntradayCandlesRequest {
    pub security_id: SecurityId,
    pub exchange_segment: ExchangeSegment,
    pub instrument: InstrumentKind,
    pub interval: CandleInterval,
    pub oi: bool,
    /// IST wall-clock time.
    #[serde(serialize_with = "ser_datetime")]
    pub from_date: NaiveDateTime,
    /// IST wall-clock time.
    #[serde(serialize_with = "ser_datetime")]
    pub to_date: NaiveDateTime,
}

impl IntradayCandlesRequest {
    pub fn equity(
        segment: ExchangeSegment,
        security_id: SecurityId,
        interval: CandleInterval,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Self {
        Self {
            security_id,
            exchange_segment: segment,
            instrument: InstrumentKind::Equity,
            interval,
            oi: false,
            from_date: from,
            to_date: to,
        }
    }
}

fn ser_date<S: Serializer>(d: &NaiveDate, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(&d.format("%Y-%m-%d"))
}

fn ser_datetime<S: Serializer>(d: &NaiveDateTime, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(&d.format("%Y-%m-%d %H:%M:%S"))
}

/// Dhan returns parallel arrays; this zips them into candles and checks that
/// the arrays line up.
#[derive(Debug, Default, Deserialize)]
struct ColumnarCandles {
    #[serde(default)]
    open: Vec<Value>,
    #[serde(default)]
    high: Vec<Value>,
    #[serde(default)]
    low: Vec<Value>,
    #[serde(default)]
    close: Vec<Value>,
    #[serde(default)]
    volume: Vec<Value>,
    #[serde(default)]
    timestamp: Vec<Value>,
    #[serde(default)]
    open_interest: Vec<Value>,
}

fn zip_candles(endpoint: &str, c: ColumnarCandles, with_oi: bool) -> Result<Vec<Candle>> {
    let n = c.timestamp.len();
    let lens = [
        c.open.len(),
        c.high.len(),
        c.low.len(),
        c.close.len(),
        c.volume.len(),
    ];
    if lens.iter().any(|&l| l != n) {
        return Err(DhanError::decode(
            endpoint,
            format!("candle arrays have different lengths: timestamp={n}, ohlcv={lens:?}"),
            b"",
        ));
    }
    let err = |field: &str, i: usize, e: String| {
        DhanError::decode(endpoint, format!("{field}[{i}]: {e}"), b"")
    };
    let dec = |v: &Value, field: &str, i: usize| -> Result<Decimal> {
        decimal_from_value(v)
            .map_err(|e| err(field, i, e))?
            .ok_or_else(|| err(field, i, "missing".into()))
    };
    let int = |v: &Value, field: &str, i: usize| -> Result<i64> {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .ok_or_else(|| err(field, i, format!("not an integer: {v}")))
    };

    let mut out = Vec::with_capacity(n);
    let rows = c
        .timestamp
        .iter()
        .zip(&c.open)
        .zip(&c.high)
        .zip(&c.low)
        .zip(&c.close)
        .zip(&c.volume)
        .enumerate();
    for (i, (((((ts, o), h), l), cl), v)) in rows {
        let open_interest = if with_oi {
            c.open_interest
                .get(i)
                .map(|x| int(x, "open_interest", i))
                .transpose()?
        } else {
            None
        };
        out.push(Candle {
            timestamp: int(ts, "timestamp", i)?,
            open: dec(o, "open", i)?,
            high: dec(h, "high", i)?,
            low: dec(l, "low", i)?,
            close: dec(cl, "close", i)?,
            volume: int(v, "volume", i)?,
            open_interest,
        });
    }
    Ok(out)
}

impl DhanClient {
    pub async fn daily_candles(&self, req: &DailyCandlesRequest) -> Result<Vec<Candle>> {
        if req.from_date >= req.to_date {
            return Err(DhanError::InvalidRequest(
                "from_date must be before to_date".into(),
            ));
        }
        let path = "/charts/historical";
        let c: ColumnarCandles = self
            .send_json(Method::POST, path, req, false, ApiCategory::Data)
            .await?;
        zip_candles(path, c, req.oi)
    }

    /// One intraday request; the range must be at most 90 days. For longer
    /// ranges use [`intraday_candles_range`](Self::intraday_candles_range).
    pub async fn intraday_candles(&self, req: &IntradayCandlesRequest) -> Result<Vec<Candle>> {
        if req.from_date >= req.to_date {
            return Err(DhanError::InvalidRequest(
                "from_date must be before to_date".into(),
            ));
        }
        if req.to_date - req.from_date > TimeDelta::days(MAX_INTRADAY_DAYS) {
            return Err(DhanError::InvalidRequest(format!(
                "intraday range is limited to {MAX_INTRADAY_DAYS} days per call"
            )));
        }
        let path = "/charts/intraday";
        let c: ColumnarCandles = self
            .send_json(Method::POST, path, req, false, ApiCategory::Data)
            .await?;
        zip_candles(path, c, req.oi)
    }

    /// Fetches any range by splitting it into ≤90-day calls. Results are
    /// sorted by time with duplicates at window edges removed.
    pub async fn intraday_candles_range(
        &self,
        req: &IntradayCandlesRequest,
    ) -> Result<Vec<Candle>> {
        let window = TimeDelta::days(MAX_INTRADAY_DAYS);
        let mut all = Vec::new();
        let mut start = req.from_date;
        while start < req.to_date {
            let end = (start + window).min(req.to_date);
            let chunk = IntradayCandlesRequest {
                from_date: start,
                to_date: end,
                ..req.clone()
            };
            all.extend(self.intraday_candles(&chunk).await?);
            start = end;
        }
        all.sort_by_key(|c| c.timestamp);
        all.dedup_by_key(|c| c.timestamp);
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zips_columns() -> Result<()> {
        let c: ColumnarCandles = serde_json::from_value(json!({
            "open": [3978, 3856.5], "high": [3978, 3925], "low": [3861, 3856],
            "close": [3879.85, 3915.9], "volume": [3937092, 1906106],
            "timestamp": [1326220200, 1326306600], "open_interest": [0, 0]
        }))
        .map_err(|e| DhanError::decode("test", e, b""))?;
        let candles = zip_candles("test", c, false)?;
        assert_eq!(candles.len(), 2);
        assert_eq!(
            candles.first().map(|c| c.close),
            Some(Decimal::new(387985, 2))
        );
        assert_eq!(candles.get(1).map(|c| c.volume), Some(1906106));
        Ok(())
    }

    #[test]
    fn rejects_ragged_columns() {
        let c = ColumnarCandles {
            open: vec![json!(1)],
            timestamp: vec![json!(1), json!(2)],
            ..ColumnarCandles::default()
        };
        assert!(zip_candles("test", c, false).is_err());
    }

    #[test]
    fn empty_response_is_empty() -> Result<()> {
        let c: ColumnarCandles =
            serde_json::from_value(json!({})).map_err(|e| DhanError::decode("t", e, b""))?;
        assert!(zip_candles("test", c, false)?.is_empty());
        Ok(())
    }

    #[test]
    fn interval_serializes_as_number() -> Result<(), serde_json::Error> {
        assert_eq!(serde_json::to_string(&CandleInterval::Min5)?, "5");
        Ok(())
    }
}
