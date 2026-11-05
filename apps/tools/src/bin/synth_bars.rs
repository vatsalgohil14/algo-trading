//! Generates synthetic 1-minute bars and loads them into QuestDB `bars_1m`.
//!
//! Synthetic rows use segment `SYNTH`, source `synthetic` and security ids
//! from 900,000,001, so they never mix with real data and are easy to drop:
//!
//! ```sql
//! DELETE FROM bars_1m WHERE source = 'synthetic';  -- or drop the partitions
//! ```

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use chrono::NaiveDate;
use clap::Parser;
use sim::synthetic::{SyntheticConfig, SyntheticMarket, sample_universe, weekdays};
use store::rows::Bar as BarRow;
use store::{Record, Store, StoreConfig};
use trading_core::Bar;

const SECURITY_ID_BASE: i32 = 900_000_000;

#[derive(Debug, Parser)]
#[command(about = "Generate synthetic 1-minute bars into QuestDB")]
struct Args {
    /// Number of stocks.
    #[arg(long, default_value_t = 20)]
    stocks: u32,
    /// First day (YYYY-MM-DD).
    #[arg(long)]
    from: NaiveDate,
    /// Last day, inclusive (YYYY-MM-DD).
    #[arg(long)]
    to: NaiveDate,
    /// Random seed; the same seed always gives the same data.
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// QuestDB address.
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
    /// Generate and summarise without writing.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    if args.from > args.to {
        return Err("--from must not be after --to".into());
    }
    if args.stocks == 0 || args.stocks > 10_000 {
        return Err("--stocks must be between 1 and 10000".into());
    }

    let profiles = sample_universe(args.stocks, args.seed);
    let segment: Arc<str> = Arc::from("SYNTH");
    let source: Arc<str> = Arc::from("synthetic");
    let symbols: Vec<Arc<str>> = profiles
        .iter()
        .map(|p| Arc::from(p.symbol.as_str()))
        .collect();
    let config = SyntheticConfig {
        seed: args.seed,
        ..SyntheticConfig::default()
    };
    let mut market = SyntheticMarket::new(config, profiles)?;

    let store = if args.dry_run {
        None
    } else {
        Some(Store::start(StoreConfig {
            addr: args.qdb.clone(),
            ..StoreConfig::default()
        })?)
    };

    let started = Instant::now();
    let (mut days, mut rows) = (0u64, 0u64);
    for date in weekdays(args.from, args.to) {
        let bars = market.session(date)?;
        if let Some(store) = &store {
            for bar in &bars {
                store.write(Record::Bar1m(to_row(bar, &segment, &source, &symbols)?))?;
            }
        }
        days += 1;
        rows += bars.len() as u64;
    }

    if let Some(store) = store {
        store.flush()?;
        let stats = store.stats();
        store.shutdown()?;
        if stats.dropped_queue_full
            + stats.rejected_invalid
            + stats.flush_errors
            + stats.server_errors
            > 0
        {
            return Err(format!("some rows were not stored: {stats:?}").into());
        }
    }

    println!(
        "{} {rows} bars ({} stocks x {days} days) in {:.1}s",
        if args.dry_run { "generated" } else { "stored" },
        args.stocks,
        started.elapsed().as_secs_f64()
    );
    for p in market.profiles() {
        let id = SECURITY_ID_BASE + p.instrument.0 as i32 + 1;
        println!(
            "  {} security_id={id} start={} vol={:.1}%",
            p.symbol,
            p.start_price,
            p.daily_vol * 100.0
        );
    }
    Ok(())
}

fn to_row(
    bar: &Bar,
    segment: &Arc<str>,
    source: &Arc<str>,
    symbols: &[Arc<str>],
) -> Result<BarRow, String> {
    let symbol = symbols
        .get(bar.instrument.index())
        .ok_or_else(|| format!("no symbol for {}", bar.instrument))?;
    let ts = bar
        .ts
        .to_utc()
        .ok_or_else(|| format!("bad timestamp {:?}", bar.ts))?;
    Ok(BarRow {
        ts,
        segment: segment.clone(),
        security_id: SECURITY_ID_BASE + bar.instrument.0 as i32 + 1,
        symbol: symbol.clone(),
        open: bar.open.as_f64(),
        high: bar.high.as_f64(),
        low: bar.low.as_f64(),
        close: bar.close.as_f64(),
        volume: i64::try_from(bar.volume).unwrap_or(i64::MAX),
        oi: None,
        source: source.clone(),
    })
}
