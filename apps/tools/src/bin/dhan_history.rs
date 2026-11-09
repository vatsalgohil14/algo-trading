//! Downloads 1-minute candles from Dhan into QuestDB `bars_1m`, and writes
//! the universe file the backtester reads.
//!
//! ```bash
//! export DHAN_CLIENT_ID=...  DHAN_ACCESS_TOKEN=...
//! scripts/questdb.sh start
//! cargo run --release -p tools --bin dhan-history -- \
//!     --symbols-file config/universes/liquid_starter.txt \
//!     --from 2022-01-01 --to 2026-10-31
//! ```
//!
//! Safe to re-run: `bars_1m` ignores rows it already has. One stock failing
//! does not stop the others; failures are listed at the end and the tool
//! exits with an error.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use clap::Parser;
use dhan::instruments::{Instrument, InstrumentMaster};
use dhan::{Candle, CandleInterval, Decimal, DhanClient, ExchangeSegment, IntradayCandlesRequest};
use rust_decimal::prelude::ToPrimitive;
use store::rows::Bar as BarRow;
use store::{Record, Store, StoreConfig};

type Error = Box<dyn std::error::Error>;

const SEGMENT: &str = "NSE_EQ";
const SOURCE: &str = "dhan_hist";

#[derive(Debug, Parser)]
#[command(about = "Download 1-minute candles from Dhan into QuestDB")]
struct Args {
    /// Comma-separated NSE symbols, e.g. RELIANCE,INFY.
    #[arg(long, value_delimiter = ',')]
    symbols: Vec<String>,
    /// File with one symbol per line; `#` starts a comment.
    #[arg(long)]
    symbols_file: Option<String>,
    /// First day (YYYY-MM-DD).
    #[arg(long)]
    from: NaiveDate,
    /// Last day, inclusive (YYYY-MM-DD).
    #[arg(long)]
    to: NaiveDate,
    /// Universe file to write for the backtester.
    #[arg(long, default_value = "data/universe.csv")]
    universe_out: String,
    /// QuestDB address.
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(&Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_symbols(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            l.split('#')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_uppercase()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn to_f64(d: Decimal) -> Option<f64> {
    d.to_f64().filter(|v| v.is_finite())
}

/// Candle as a database row. `None` if it cannot be represented.
fn to_row(c: &Candle, symbol: &Arc<str>, security_id: i32) -> Option<BarRow> {
    let ts: DateTime<Utc> = DateTime::from_timestamp(c.timestamp, 0)?;
    Some(BarRow {
        ts,
        segment: Arc::from(SEGMENT),
        security_id,
        symbol: symbol.clone(),
        open: to_f64(c.open)?,
        high: to_f64(c.high)?,
        low: to_f64(c.low)?,
        close: to_f64(c.close)?,
        volume: c.volume,
        oi: None,
        source: Arc::from(SOURCE),
    })
}

/// `symbol,security_id,tick_paise,lot_size`, as the backtester reads it.
fn universe_line(symbol: &str, security_id: i32, tick: Decimal, lot: Decimal) -> Option<String> {
    let paise = (tick * Decimal::ONE_HUNDRED).round().to_i64()?;
    let lot = lot.to_i64().filter(|l| *l > 0).unwrap_or(1);
    (paise > 0).then(|| format!("{symbol},{security_id},{paise},{lot}"))
}

async fn download(
    client: &DhanClient,
    store: &Store,
    instrument: &Instrument,
    symbol: &str,
    (from, to): (chrono::NaiveDateTime, chrono::NaiveDateTime),
) -> Result<(u64, i32), Error> {
    let security_id = i32::try_from(instrument.security_id.0)?;
    let request = IntradayCandlesRequest::equity(
        ExchangeSegment::NseEq,
        instrument.security_id,
        CandleInterval::Min1,
        from,
        to,
    );
    let candles = client.intraday_candles_range(&request).await?;
    let name: Arc<str> = Arc::from(symbol);
    let mut stored = 0;
    for candle in &candles {
        let row = to_row(candle, &name, security_id)
            .ok_or_else(|| format!("candle at {} cannot be stored", candle.timestamp))?;
        store.write(Record::Bar1m(row))?;
        stored += 1;
    }
    Ok((stored, security_id))
}

async fn run(args: &Args) -> Result<(), Error> {
    if args.from > args.to {
        return Err("--from must not be after --to".into());
    }
    let mut symbols = args.symbols.clone();
    if let Some(path) = &args.symbols_file {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        symbols.extend(parse_symbols(&text));
    }
    if symbols.is_empty() {
        return Err("give --symbols or --symbols-file".into());
    }

    let client = DhanClient::from_env()?;
    let master = InstrumentMaster::download(&reqwest::Client::new()).await?;
    let store = Store::start(StoreConfig {
        addr: args.qdb.clone(),
        ..StoreConfig::default()
    })?;
    let open = NaiveTime::from_hms_opt(9, 15, 0).ok_or("bad time")?;
    let close = NaiveTime::from_hms_opt(15, 30, 0).ok_or("bad time")?;
    let range = (args.from.and_time(open), args.to.and_time(close));

    let mut universe = String::from("symbol,security_id,tick_paise,lot_size\n");
    let mut failed = Vec::new();
    for (n, symbol) in symbols.iter().enumerate() {
        let Some(instrument) = master.nse_equity(symbol) else {
            println!("[{}/{}] {symbol}: not found on NSE", n + 1, symbols.len());
            failed.push(symbol.clone());
            continue;
        };
        match download(&client, &store, instrument, symbol, range).await {
            Ok((0, _)) => {
                println!("[{}/{}] {symbol}: no data", n + 1, symbols.len());
                failed.push(symbol.clone());
            }
            Ok((rows, security_id)) => {
                println!("[{}/{}] {symbol}: {rows} bars", n + 1, symbols.len());
                let line = universe_line(
                    symbol,
                    security_id,
                    instrument.tick_size(),
                    instrument.lot_size,
                )
                .ok_or_else(|| format!("{symbol}: bad tick or lot size"))?;
                writeln!(universe, "{line}")?;
            }
            Err(e) => {
                println!("[{}/{}] {symbol}: {e}", n + 1, symbols.len());
                failed.push(symbol.clone());
            }
        }
    }

    store.flush()?;
    let stats = store.stats();
    store.shutdown()?;
    if stats.dropped_queue_full + stats.rejected_invalid + stats.flush_errors + stats.server_errors
        > 0
    {
        return Err(format!("some rows were not stored: {stats:?}").into());
    }

    if let Some(parent) = std::path::Path::new(&args.universe_out).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.universe_out, universe)?;
    println!("Universe written to {}", args.universe_out);
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} failed: {}", failed.len(), failed.join(", ")).into())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn symbols_file() {
        let got = parse_symbols("# header\nreliance\n\n INFY  # note\n");
        assert_eq!(got, ["RELIANCE", "INFY"]);
    }

    #[test]
    fn candle_becomes_a_row() {
        let c = Candle {
            timestamp: 1_736_394_300,
            open: d("100.05"),
            high: d("101"),
            low: d("99.5"),
            close: d("100.5"),
            volume: 1234,
            open_interest: None,
        };
        let row = to_row(&c, &Arc::from("X"), 7).unwrap();
        assert_eq!(row.ts.timestamp(), 1_736_394_300);
        assert_eq!((row.open, row.high, row.low), (100.05, 101.0, 99.5));
        assert_eq!((row.volume, row.security_id), (1234, 7));
        assert_eq!(&*row.segment, "NSE_EQ");
    }

    #[test]
    fn universe_lines() {
        assert_eq!(
            universe_line("SBIN", 3045, d("0.05"), d("1")).unwrap(),
            "SBIN,3045,5,1"
        );
        assert!(universe_line("X", 1, d("0"), d("1")).is_none());
    }
}
