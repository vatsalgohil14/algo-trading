//! Small queries over QWP: the schema check before writing, and the live
//! tests. (Tools read bars through QuestDB's CSV export instead.)

use questdb::egress::column::ColumnView;
use questdb::egress::reader::Reader;

use crate::{Result, StoreConfig, StoreError};

/// A QWP query connection. Not thread-safe; open one per thread.
pub struct QueryClient {
    reader: Reader,
}

impl std::fmt::Debug for QueryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryClient").finish_non_exhaustive()
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
