//! One struct per table in `deploy/questdb/schema.sql`. Field names and
//! types mirror the columns exactly; keep the two in sync.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use questdb::ingress::{Buffer, TimestampMicros};

/// Shared text for symbol-like fields.
pub type Text = Arc<str>;

fn micros(ts: &DateTime<Utc>) -> TimestampMicros {
    TimestampMicros::new(ts.timestamp_micros())
}

type QResult = questdb::Result<()>;

/// A 1-minute or daily candle.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    /// Candle start.
    pub ts: DateTime<Utc>,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub oi: Option<i64>,
    /// Where it came from, e.g. `dhan_hist`, `live`.
    pub source: Text,
}

impl Bar {
    fn append(&self, buf: &mut Buffer, table: &str) -> QResult {
        buf.table(table)?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .symbol("source", &*self.source)?
            .column_i32("security_id", self.security_id)?
            .column_f64("open", self.open)?
            .column_f64("high", self.high)?
            .column_f64("low", self.low)?
            .column_f64("close", self.close)?
            .column_i64("volume", self.volume)?
            .column_i64_opt("oi", self.oi)?
            .at(micros(&self.ts))
    }
}

/// Any row the store can write.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Bar1m(Bar),
    BarDaily(Bar),
}

impl Record {
    pub fn table(&self) -> &'static str {
        match self {
            Record::Bar1m(_) => "bars_1m",
            Record::BarDaily(_) => "bars_daily",
        }
    }

    /// Appends this row to a QWP buffer.
    pub fn append_to(&self, buf: &mut Buffer) -> questdb::Result<()> {
        match self {
            Record::Bar1m(b) | Record::BarDaily(b) => b.append(buf, self.table()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_encode_without_a_server() {
        let bar = Bar {
            ts: DateTime::from_timestamp(1_762_140_600, 0).unwrap(),
            segment: "NSE_EQ".into(),
            security_id: 2885,
            symbol: "RELIANCE".into(),
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10,
            oi: None,
            source: "test".into(),
        };
        let mut buf = Buffer::new_qwp_ws();
        let rows = [Record::Bar1m(bar.clone()), Record::BarDaily(bar)];
        for r in &rows {
            r.append_to(&mut buf).unwrap();
        }
        assert_eq!(buf.row_count(), 2);
        assert_eq!(rows[1].table(), "bars_daily");
    }
}
