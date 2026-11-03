//! Reads over QWP. Results arrive as compressed column batches, which is much
//! faster than row-by-row protocols when loading years of bars for a
//! backtest.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use questdb::egress::Bind;
use questdb::egress::column::ColumnView;
use questdb::egress::reader::Reader;

use crate::rows::{Bar, Text};
use crate::{Result, StoreConfig, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarTable {
    OneMinute,
    Daily,
}

impl BarTable {
    const fn name(self) -> &'static str {
        match self {
            BarTable::OneMinute => "bars_1m",
            BarTable::Daily => "bars_daily",
        }
    }
}

/// A QWP query connection. Not thread-safe; open one per thread.
pub struct QueryClient {
    reader: Reader,
}

impl std::fmt::Debug for QueryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryClient").finish_non_exhaustive()
    }
}

/// Reuses the previous `Arc<str>` while consecutive rows repeat a value, so
/// a million bars of one stock share one allocation per text column.
#[derive(Default)]
struct Interner(Option<Text>);

impl Interner {
    fn get(&mut self, s: &str) -> Text {
        match &self.0 {
            Some(prev) if &**prev == s => Arc::clone(prev),
            _ => {
                let t: Text = s.into();
                self.0 = Some(Arc::clone(&t));
                t
            }
        }
    }
}

fn shape(msg: impl Into<String>) -> StoreError {
    StoreError::Shape(msg.into())
}

impl QueryClient {
    pub fn connect(config: &StoreConfig) -> Result<Self> {
        Ok(Self {
            reader: Reader::from_conf(config.reader_conf())?,
        })
    }

    /// Bars for one instrument in `[from, to)`, oldest first.
    pub fn bars(
        &mut self,
        table: BarTable,
        security_id: i32,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Bar>> {
        let sql = format!(
            "SELECT ts, segment, security_id, symbol, open, high, low, close, volume, oi, source \
             FROM {} WHERE security_id = $1 AND ts >= $2 AND ts < $3 ORDER BY ts",
            table.name()
        );
        let mut cursor = self
            .reader
            .prepare(sql)
            .bind(Bind::I32(security_id))
            .bind(Bind::TimestampMicros(from.timestamp_micros()))
            .bind(Bind::TimestampMicros(to.timestamp_micros()))
            .execute()?;

        let (mut seg_i, mut sym_i, mut src_i) = (
            Interner::default(),
            Interner::default(),
            Interner::default(),
        );
        let mut out = Vec::new();
        while let Some(batch) = cursor.next_batch()? {
            let n = batch.row_count();
            let (
                ColumnView::Timestamp(ts),
                ColumnView::Symbol(seg),
                ColumnView::Int(sid),
                ColumnView::Symbol(sym),
                ColumnView::Double(open),
                ColumnView::Double(high),
                ColumnView::Double(low),
                ColumnView::Double(close),
                ColumnView::Long(volume),
                ColumnView::Long(oi),
                ColumnView::Symbol(src),
            ) = (
                batch.column(0)?,
                batch.column(1)?,
                batch.column(2)?,
                batch.column(3)?,
                batch.column(4)?,
                batch.column(5)?,
                batch.column(6)?,
                batch.column(7)?,
                batch.column(8)?,
                batch.column(9)?,
                batch.column(10)?,
            )
            else {
                return Err(shape("bar columns have unexpected types; check schema.sql"));
            };
            out.reserve(n);
            for r in 0..n {
                let ts_us = ts.value(r);
                let ts = DateTime::from_timestamp_micros(ts_us)
                    .ok_or_else(|| shape(format!("timestamp {ts_us} out of range")))?;
                out.push(Bar {
                    ts,
                    segment: seg_i.get(seg.resolve(r).unwrap_or_default()),
                    security_id: sid.value(r),
                    symbol: sym_i.get(sym.resolve(r).unwrap_or_default()),
                    open: open.value(r),
                    high: high.value(r),
                    low: low.value(r),
                    close: close.value(r),
                    volume: if volume.is_null(r) {
                        0
                    } else {
                        volume.value(r)
                    },
                    oi: (!oi.is_null(r)).then(|| oi.value(r)),
                    source: src_i.get(src.resolve(r).unwrap_or_default()),
                });
            }
        }
        Ok(out)
    }

    /// Runs a query returning two text columns, e.g. `table_columns()`.
    pub fn string_pairs(&mut self, sql: &str) -> Result<Vec<(String, String)>> {
        fn text(col: &ColumnView<'_>, r: usize) -> Option<String> {
            match col {
                ColumnView::Varchar(c) => c.value(r).map(str::to_owned),
                ColumnView::Symbol(c) => c.resolve(r).map(str::to_owned),
                _ => None,
            }
        }
        let mut cursor = self.reader.prepare(sql).execute()?;
        let mut out = Vec::new();
        while let Some(batch) = cursor.next_batch()? {
            let (a, b) = (batch.column(0)?, batch.column(1)?);
            for r in 0..batch.row_count() {
                match (text(&a, r), text(&b, r)) {
                    (Some(x), Some(y)) => out.push((x, y)),
                    _ => return Err(shape("expected two text columns")),
                }
            }
        }
        Ok(out)
    }

    /// Runs a statement that returns no rows (DDL, `ALTER TABLE`, ...).
    pub fn execute(&mut self, sql: &str) -> Result<()> {
        let mut cursor = self.reader.prepare(sql).execute()?;
        while cursor.next_batch()?.is_some() {}
        Ok(())
    }

    /// Runs a query returning a single integer (e.g. `SELECT count() ...`).
    pub fn scalar_i64(&mut self, sql: &str) -> Result<i64> {
        let mut cursor = self.reader.prepare(sql).execute()?;
        let Some(batch) = cursor.next_batch()? else {
            return Err(shape("query returned no rows"));
        };
        if batch.row_count() == 0 {
            return Err(shape("query returned no rows"));
        }
        let value = match batch.column(0)? {
            ColumnView::Long(c) => c.value(0),
            ColumnView::Int(c) => i64::from(c.value(0)),
            other => {
                return Err(shape(format!(
                    "expected an integer, got {:?}",
                    other.kind()
                )));
            }
        };
        // Drain the rest so the connection can be reused.
        while cursor.next_batch()?.is_some() {}
        Ok(value)
    }
}
