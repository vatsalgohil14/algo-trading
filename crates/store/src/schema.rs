//! The columns this crate writes, per table, and a check that the live
//! database has them.
//!
//! Why this matters: QWP rejects a whole batch if any row in it doesn't
//! match the table, and the sender then stops accepting data. Checking the
//! schema before trading starts turns that mid-day failure into a loud
//! startup error. A unit test keeps this list in sync with
//! `deploy/questdb/schema.sql`.

use crate::query::QueryClient;
use crate::{Result, StoreError};

/// `(table, [(column, QuestDB type as reported by table_columns())])`.
pub const TABLES: &[(&str, &[(&str, &str)])] = &[
    (
        "instruments",
        &[
            ("ts", "TIMESTAMP"),
            ("segment", "SYMBOL"),
            ("security_id", "INT"),
            ("symbol", "SYMBOL"),
            ("series", "SYMBOL"),
            ("isin", "VARCHAR"),
            ("name", "VARCHAR"),
            ("tick_size", "DOUBLE"),
            ("lot_size", "INT"),
            ("upper_circuit", "DOUBLE"),
            ("lower_circuit", "DOUBLE"),
            ("asm_gsm", "BOOLEAN"),
            ("mtf_leverage", "DOUBLE"),
        ],
    ),
    ("bars_1m", BAR_COLUMNS),
    ("bars_daily", BAR_COLUMNS),
    (
        "ticks",
        &[
            ("ts", "TIMESTAMP"),
            ("segment", "SYMBOL"),
            ("security_id", "INT"),
            ("symbol", "SYMBOL"),
            ("ltp", "DOUBLE"),
            ("ltq", "INT"),
            ("ltt", "TIMESTAMP"),
            ("volume", "LONG"),
            ("atp", "DOUBLE"),
            ("bid", "DOUBLE"),
            ("ask", "DOUBLE"),
            ("bid_qty", "INT"),
            ("ask_qty", "INT"),
            ("total_buy_qty", "LONG"),
            ("total_sell_qty", "LONG"),
        ],
    ),
    (
        "runs",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("strategy", "SYMBOL"),
            ("broker", "SYMBOL"),
            ("git_sha", "VARCHAR"),
            ("params", "VARCHAR"),
            ("notes", "VARCHAR"),
        ],
    ),
    (
        "signals",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("strategy", "SYMBOL"),
            ("segment", "SYMBOL"),
            ("security_id", "INT"),
            ("symbol", "SYMBOL"),
            ("side", "SYMBOL"),
            ("entry", "DECIMAL(12,2)"),
            ("stop", "DECIMAL(12,2)"),
            ("target", "DECIMAL(12,2)"),
            ("qty", "INT"),
            ("decision", "SYMBOL"),
            ("reason", "VARCHAR"),
        ],
    ),
    (
        "orders",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("strategy", "SYMBOL"),
            ("correlation_id", "VARCHAR"),
            ("order_id", "VARCHAR"),
            ("segment", "SYMBOL"),
            ("security_id", "INT"),
            ("symbol", "SYMBOL"),
            ("side", "SYMBOL"),
            ("product", "SYMBOL"),
            ("order_type", "SYMBOL"),
            ("qty", "INT"),
            ("price", "DECIMAL(12,2)"),
            ("trigger_price", "DECIMAL(12,2)"),
            ("status", "SYMBOL"),
            ("filled_qty", "INT"),
            ("avg_price", "DECIMAL(12,2)"),
            ("reason", "VARCHAR"),
        ],
    ),
    (
        "fills",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("strategy", "SYMBOL"),
            ("correlation_id", "VARCHAR"),
            ("order_id", "VARCHAR"),
            ("trade_id", "VARCHAR"),
            ("segment", "SYMBOL"),
            ("security_id", "INT"),
            ("symbol", "SYMBOL"),
            ("side", "SYMBOL"),
            ("qty", "INT"),
            ("price", "DECIMAL(12,2)"),
            ("expected_price", "DECIMAL(12,2)"),
            ("charges", "DECIMAL(12,2)"),
        ],
    ),
    (
        "positions",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("strategy", "SYMBOL"),
            ("segment", "SYMBOL"),
            ("security_id", "INT"),
            ("symbol", "SYMBOL"),
            ("net_qty", "INT"),
            ("avg_price", "DECIMAL(12,2)"),
            ("ltp", "DOUBLE"),
            ("realized_pnl", "DECIMAL(14,2)"),
            ("unrealized_pnl", "DECIMAL(14,2)"),
        ],
    ),
    (
        "pnl_daily",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("strategy", "SYMBOL"),
            ("trades", "INT"),
            ("wins", "INT"),
            ("losses", "INT"),
            ("gross_pnl", "DECIMAL(14,2)"),
            ("charges", "DECIMAL(14,2)"),
            ("net_pnl", "DECIMAL(14,2)"),
            ("max_drawdown", "DECIMAL(14,2)"),
            ("turnover", "DECIMAL(16,2)"),
        ],
    ),
    (
        "system_events",
        &[
            ("ts", "TIMESTAMP"),
            ("run_id", "SYMBOL"),
            ("mode", "SYMBOL"),
            ("level", "SYMBOL"),
            ("component", "SYMBOL"),
            ("event", "SYMBOL"),
            ("message", "VARCHAR"),
        ],
    ),
];

const BAR_COLUMNS: &[(&str, &str)] = &[
    ("ts", "TIMESTAMP"),
    ("segment", "SYMBOL"),
    ("security_id", "INT"),
    ("symbol", "SYMBOL"),
    ("open", "DOUBLE"),
    ("high", "DOUBLE"),
    ("low", "DOUBLE"),
    ("close", "DOUBLE"),
    ("volume", "LONG"),
    ("oi", "LONG"),
    ("source", "SYMBOL"),
];

/// Compares [`TABLES`] with the live database and reports every problem at
/// once. Extra columns in the database are fine; missing or differently
/// typed ones are not.
pub fn verify(q: &mut QueryClient) -> Result<()> {
    let mut problems = Vec::new();
    for (table, columns) in TABLES {
        let actual = match q.string_pairs(&format!(
            "SELECT \"column\", type FROM table_columns('{table}')"
        )) {
            Ok(rows) => rows,
            Err(e) => {
                problems.push(format!("{table}: cannot read columns ({e})"));
                continue;
            }
        };
        if actual.is_empty() {
            problems.push(format!("{table}: table missing"));
            continue;
        }
        for (col, ty) in *columns {
            match actual.iter().find(|(c, _)| c == col) {
                None => problems.push(format!("{table}.{col}: column missing")),
                Some((_, actual_ty)) if !actual_ty.eq_ignore_ascii_case(ty) => {
                    problems.push(format!("{table}.{col}: type is {actual_ty}, expected {ty}"));
                }
                Some(_) => {}
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(StoreError::Config(format!(
            "database schema does not match (run scripts/questdb.sh init?): {}",
            problems.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA_SQL: &str = include_str!("../../../deploy/questdb/schema.sql");

    /// Pulls `(column, TYPE)` pairs out of each CREATE TABLE in schema.sql.
    fn parse_schema_sql() -> Vec<(String, Vec<(String, String)>)> {
        let mut out = Vec::new();
        let mut current: Option<(String, Vec<(String, String)>)> = None;
        for raw in SCHEMA_SQL.lines() {
            let line = raw.split("--").next().unwrap_or("").trim();
            if let Some(rest) = line.strip_prefix("CREATE TABLE IF NOT EXISTS ") {
                let name = rest.trim_end_matches('(').trim().to_owned();
                current = Some((name, Vec::new()));
            } else if line.starts_with(')') {
                if let Some(t) = current.take() {
                    out.push(t);
                }
            } else if let Some((_, cols)) = current.as_mut()
                && !line.is_empty()
            {
                let def = line.trim_end_matches(',');
                let mut parts = def.splitn(2, ' ');
                let col = parts.next().unwrap_or("").to_owned();
                let rest = parts.next().unwrap_or("");
                // Type is the first word, plus "(p, s)" for DECIMAL.
                let ty = if rest.starts_with("DECIMAL") {
                    rest.split(')').next().unwrap_or("").replace(' ', "") + ")"
                } else {
                    rest.split_whitespace().next().unwrap_or("").to_owned()
                };
                cols.push((col, ty));
            }
        }
        out
    }

    #[test]
    fn code_matches_schema_sql() {
        let sql = parse_schema_sql();
        assert_eq!(
            sql.len(),
            TABLES.len(),
            "table count differs from schema.sql"
        );
        for (table, columns) in TABLES {
            let (_, sql_cols) = sql
                .iter()
                .find(|(t, _)| t == table)
                .unwrap_or_else(|| panic!("{table} not in schema.sql"));
            let ours: Vec<(String, String)> = columns
                .iter()
                .map(|(c, t)| ((*c).to_owned(), (*t).to_owned()))
                .collect();
            assert_eq!(&ours, sql_cols, "columns of {table} differ from schema.sql");
        }
    }
}
