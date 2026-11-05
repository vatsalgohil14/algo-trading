//! Measures indicator update speed on synthetic data:
//! `cargo run --release -p indicators --example throughput`

use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

use chrono::NaiveDate;
use indicators::{BankBuilder, Registry};
use sim::synthetic::{SyntheticConfig, SyntheticMarket, sample_universe, weekdays};

fn main() -> Result<(), Box<dyn Error>> {
    let stocks = 200;
    let from = NaiveDate::from_ymd_opt(2025, 1, 1).ok_or("bad date")?;
    let to = NaiveDate::from_ymd_opt(2025, 1, 31).ok_or("bad date")?;
    let mut market = SyntheticMarket::new(SyntheticConfig::default(), sample_universe(stocks, 1))?;
    let mut bars = Vec::new();
    for date in weekdays(from, to) {
        bars.extend(market.session(date)?);
    }

    let mut builder = BankBuilder::new(Registry::builtin());
    let specs = [
        "sma(20)",
        "ema(9)",
        "ema(21)",
        "atr(14)",
        "rsi(14)",
        "vwap",
        "orb(15).high",
        "orb(15).low",
        "prev_day.close",
    ];
    let handles = specs
        .iter()
        .map(|s| builder.require(s))
        .collect::<Result<Vec<_>, _>>()?;
    let mut bank = builder.build(stocks as usize)?;
    let distinct = bank.specs().count();

    let start = Instant::now();
    let mut checksum = 0.0;
    for bar in &bars {
        bank.update(bar)?;
        for h in &handles {
            checksum += bank.get(bar.instrument, *h).unwrap_or(0.0);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    black_box(checksum);

    println!(
        "{} bars x {distinct} indicators ({} reads per bar): {:.2}s, {:.1}M bars/s, {:.0} ns per bar",
        bars.len(),
        handles.len(),
        secs,
        bars.len() as f64 / secs / 1e6,
        secs * 1e9 / bars.len() as f64
    );
    Ok(())
}
