//! Downloads daily candles for every NSE stock (or a list) from Dhan into
//! QuestDB `bars_daily`, and writes a universe file of everything stored.
//!
//! ```bash
//! set -a; source .env; set +a
//! scripts/questdb.sh start
//! cargo run --release -p tools --bin dhan-daily                 # all NSE stocks, 2008 on
//! cargo run --release -p tools --bin dhan-daily -- --symbols RELIANCE,INFY
//! ```
//!
//! "Every NSE stock" means every company share in Dhan's instrument list:
//! NSE equity segment, series EQ or BE, ISIN starting `INE` (Indian
//! companies; ETFs, funds and bonds have other prefixes). Companies that
//! were delisted or merged away are not in that list, so they are missing.
//!
//! ETFs and other funds asked for with `--symbols` are stored with source
//! `dhan_daily_fund`; company shares with `dhan_daily`.
//!
//! Each row is stamped at midnight IST of its trading day. Safe to re-run:
//! `bars_daily` keeps one row per day and stock. One stock failing does not
//! stop the others; failures are listed at the end.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use clap::Parser;
use dhan::instruments::{Instrument, InstrumentMaster};
use dhan::token::connect_from_env;
use dhan::{Candle, DailyCandlesRequest, Decimal, DhanClient, ExchangeSegment};
use rust_decimal::prelude::ToPrimitive;
use store::rows::Bar as BarRow;
use store::{Record, Store, StoreConfig};

type Error = Box<dyn std::error::Error>;

const SEGMENT: &str = "NSE_EQ";
/// `source` for company shares; ETFs and other funds get [`SOURCE_FUND`],
/// so stock research can leave them out.
const SOURCE: &str = "dhan_daily";
const SOURCE_FUND: &str = "dhan_daily_fund";

#[derive(Debug, Parser)]
#[command(about = "Download daily candles for NSE stocks from Dhan into QuestDB")]
struct Args {
    /// Only these symbols (comma-separated). Default: every NSE stock.
    #[arg(long, value_delimiter = ',')]
    symbols: Vec<String>,
    /// Stop after this many stocks (for a trial run).
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, default_value = "2008-01-01")]
    from: NaiveDate,
    /// Last day, inclusive. Default: today.
    #[arg(long)]
    to: Option<NaiveDate>,
    #[arg(long, default_value = "data/universe_nse.csv")]
    universe_out: String,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

/// A company share listed on NSE: equity segment, normal or trade-for-trade
/// series, Indian company ISIN.
fn is_stock(i: &Instrument) -> bool {
    i.exchange_segment() == Some(ExchangeSegment::NseEq)
        && i.instrument.eq_ignore_ascii_case("EQUITY")
        && matches!(i.series.as_deref(), Some("EQ" | "BE"))
        && i.isin.as_deref().is_some_and(|s| s.starts_with("INE"))
}

/// Dhan's DH-907 answer: no data for that stock and window.
fn is_no_data(e: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        e.downcast_ref::<dhan::DhanError>(),
        Some(dhan::DhanError::Api { code: Some(c), .. }) if c == "DH-907"
    )
}

fn to_f64(d: Decimal) -> Option<f64> {
    d.to_f64().filter(|v| v.is_finite() && *v > 0.0)
}

/// IST is UTC+5:30.
const IST_OFFSET: chrono::TimeDelta = chrono::TimeDelta::minutes(330);

/// The IST date of a Unix time in seconds.
fn ist_date(epoch: i64) -> Option<NaiveDate> {
    DateTime::from_timestamp(epoch, 0).map(|t| (t + IST_OFFSET).date_naive())
}

/// The candle's IST trading day, stamped at midnight IST.
fn day_start(epoch: i64) -> Option<DateTime<Utc>> {
    let date = ist_date(epoch)?;
    Some(date.and_time(NaiveTime::MIN).and_utc() - IST_OFFSET)
}

fn to_row(c: &Candle, symbol: &Arc<str>, security_id: i32, source: &'static str) -> Option<BarRow> {
    let (open, high, low, close) = (
        to_f64(c.open)?,
        to_f64(c.high)?,
        to_f64(c.low)?,
        to_f64(c.close)?,
    );
    let valid = low <= open.min(close) && high >= open.max(close) && c.volume >= 0;
    valid.then(|| {
        Some(BarRow {
            ts: day_start(c.timestamp)?,
            segment: Arc::from(SEGMENT),
            security_id,
            symbol: symbol.clone(),
            open,
            high,
            low,
            close,
            volume: c.volume,
            oi: None,
            source: Arc::from(source),
        })
    })?
}

/// `symbol,security_id,tick_paise,lot_size`, as the backtester reads it.
fn universe_line(symbol: &str, security_id: i32, tick: Decimal, lot: Decimal) -> Option<String> {
    let paise = (tick * Decimal::ONE_HUNDRED).round().to_i64()?;
    let lot = lot.to_i64().filter(|l| *l > 0).unwrap_or(1);
    (paise > 0).then(|| format!("{symbol},{security_id},{paise},{lot}"))
}

/// Downloads one stock. Returns (rows stored, rows refused, first day).
async fn download(
    client: &DhanClient,
    store: &Store,
    inst: &Instrument,
    from: NaiveDate,
    to_exclusive: NaiveDate,
) -> Result<(u64, u64, Option<NaiveDate>), Error> {
    let security_id = i32::try_from(inst.security_id.0)?;
    let req =
        DailyCandlesRequest::equity(ExchangeSegment::NseEq, inst.security_id, from, to_exclusive);
    let candles = client.daily_candles(&req).await?;
    let symbol: Arc<str> = Arc::from(inst.trading_symbol());
    let (mut stored, mut refused) = (0, 0);
    let mut first = None;
    let source = if is_stock(inst) { SOURCE } else { SOURCE_FUND };
    for c in &candles {
        match to_row(c, &symbol, security_id, source) {
            Some(row) => {
                if first.is_none() {
                    first = ist_date(c.timestamp);
                }
                store.write(Record::BarDaily(row))?;
                stored += 1;
            }
            None => refused += 1,
        }
    }
    Ok((stored, refused, first))
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

async fn run(args: &Args) -> Result<(), Error> {
    let to = args.to.unwrap_or_else(|| Utc::now().date_naive());
    if args.from > to {
        return Err("--from must not be after --to".into());
    }
    let to_exclusive = to.succ_opt().ok_or("bad --to")?;

    let client = connect_from_env().await?;
    let master = InstrumentMaster::download(&reqwest::Client::new()).await?;
    let mut stocks: Vec<&Instrument> = if args.symbols.is_empty() {
        master.iter().filter(|i| is_stock(i)).collect()
    } else {
        let mut v = Vec::new();
        for s in &args.symbols {
            v.push(
                master
                    .nse_equity(&s.to_ascii_uppercase())
                    .ok_or_else(|| format!("{s}: not found on NSE"))?,
            );
        }
        v
    };
    stocks.sort_by(|a, b| a.trading_symbol().cmp(b.trading_symbol()));
    stocks.dedup_by_key(|i| i.security_id);
    if let Some(n) = args.limit {
        stocks.truncate(n);
    }
    println!("{} stocks, {} to {to}", stocks.len(), args.from);

    let store = Store::start(StoreConfig {
        addr: args.qdb.clone(),
        ..StoreConfig::default()
    })?;
    let mut universe = String::from("symbol,security_id,tick_paise,lot_size\n");
    let (mut failed, mut empty) = (Vec::new(), 0usize);
    let (mut total, mut refused_total) = (0u64, 0u64);
    let started = std::time::Instant::now();
    for (n, inst) in stocks.iter().enumerate() {
        let symbol = inst.trading_symbol();
        match download(&client, &store, inst, args.from, to_exclusive).await {
            Ok((0, _, _)) => empty += 1,
            Ok((rows, refused, first)) => {
                total += rows;
                refused_total += refused;
                let id = i32::try_from(inst.security_id.0)?;
                if let Some(line) = universe_line(symbol, id, inst.tick_size(), inst.lot_size) {
                    writeln!(universe, "{line}")?;
                }
                if n % 100 == 0 || args.limit.is_some() {
                    println!(
                        "[{}/{}] {symbol}: {rows} days from {}{} ({:.0}s)",
                        n + 1,
                        stocks.len(),
                        first.map_or(String::from("?"), |d| d.to_string()),
                        if refused > 0 {
                            format!(", {refused} invalid refused")
                        } else {
                            String::new()
                        },
                        started.elapsed().as_secs_f64()
                    );
                }
            }
            // DH-907: no trades in the window (a very thin or suspended stock).
            Err(e) if is_no_data(e.as_ref()) => empty += 1,
            Err(e) => {
                println!("[{}/{}] {symbol}: {e}", n + 1, stocks.len());
                failed.push(symbol.to_owned());
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
    println!(
        "Stored {total} daily candles for {} stocks ({refused_total} invalid refused, {empty} with no data) in {:.0}s. Universe: {}",
        stocks.len() - empty - failed.len(),
        started.elapsed().as_secs_f64(),
        args.universe_out
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} failed: {}", failed.len(), failed.join(", ")).into())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn dh_907_means_no_data() {
        let api = |code: &str| -> Error {
            Box::new(dhan::DhanError::Api {
                status: 400,
                kind: dhan::ErrorKind::Other,
                code: Some(code.to_owned()),
                error_type: None,
                message: "no data present".into(),
            })
        };
        assert!(is_no_data(api("DH-907").as_ref()));
        assert!(!is_no_data(api("DH-904").as_ref()));
        let other: Error = "boom".into();
        assert!(!is_no_data(other.as_ref()));
    }

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn candle(ts: i64, o: &str, h: &str, l: &str, c: &str, v: i64) -> Candle {
        Candle {
            timestamp: ts,
            open: d(o),
            high: d(h),
            low: d(l),
            close: d(c),
            volume: v,
            open_interest: None,
        }
    }

    #[test]
    fn rows_are_stamped_at_ist_midnight() {
        // 2025-01-06 00:00 IST = 2025-01-05 18:30 UTC; any time that day maps there.
        let row = to_row(
            &candle(1_736_101_800 + 3_600, "100", "101", "99", "100.5", 10),
            &Arc::from("X"),
            1,
            SOURCE,
        )
        .unwrap();
        assert_eq!(row.ts.to_rfc3339(), "2025-01-05T18:30:00+00:00");
        assert_eq!(&*row.source, "dhan_daily");
    }

    #[test]
    fn invalid_candles_are_refused() {
        let s: Arc<str> = Arc::from("X");
        assert!(
            to_row(
                &candle(1_736_101_800, "100", "99", "98", "99", 1),
                &s,
                1,
                SOURCE
            )
            .is_none()
        ); // open above high
        assert!(to_row(&candle(1_736_101_800, "0", "1", "0", "1", 1), &s, 1, SOURCE).is_none()); // zero price
        assert!(
            to_row(
                &candle(1_736_101_800, "100", "101", "99", "100", -5),
                &s,
                1,
                SOURCE
            )
            .is_none()
        );
    }
}
