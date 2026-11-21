//! This month's rotation: what to hold, and the orders that get there from
//! what you hold now (real holdings; `rotation-paper` keeps a paper account
//! and prints the same orders). Uses the `rotation` crate, the same code as the
//! backtest. It only prints; it places no orders.
//!
//! ```bash
//! scripts/questdb.sh start
//! cargo run --release -p tools --bin dhan-daily -- --from 2024-01-01   # bring bars_daily up to date
//! cargo run --release -p tools --bin rotation-plan -- --strategy momentum_low_vol --capital 1000000
//! cargo run --release -p tools --bin rotation-plan -- --strategy momentum --capital 1000000 \
//!     --holdings my_holdings.csv --out data/plan.csv
//! ```
//!
//! `--holdings` is a CSV of `symbol,shares` (header optional). Without it,
//! the plan assumes you hold nothing.

use std::fmt::Write as _;
use std::process::ExitCode;

use chrono::{NaiveDate, Utc};
use clap::Parser;
use rotation::{Panel, RotationConfig, Side, UniverseConfig, build, plan, trades};

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Parser)]
#[command(about = "Target holdings and orders for this month's rotation (prints only)")]
struct Args {
    /// `momentum` or `momentum_low_vol`.
    #[arg(long, default_value = "momentum_low_vol")]
    strategy: String,
    /// Strategy settings as JSON, e.g. '{"lookback_months":12,"skip_months":1}'.
    #[arg(long, default_value = "{}")]
    params: String,
    /// Rupees to invest in total.
    #[arg(long)]
    capital: f64,
    #[arg(long, default_value_t = 30)]
    hold: usize,
    /// Rank buffer: keep stocks you hold while they rank within this many.
    /// Use the same number as `--hold` to sell as soon as they leave it.
    #[arg(long, default_value_t = 60)]
    exit_rank: usize,
    #[arg(long, default_value_t = 500)]
    universe_size: usize,
    /// Plan as of this day's close. Default: the latest day in bars_daily.
    #[arg(long)]
    date: Option<NaiveDate>,
    /// Current holdings, `symbol,shares`.
    #[arg(long)]
    holdings: Option<String>,
    /// Also write the targets and orders to this CSV.
    #[arg(long)]
    out: Option<String>,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

async fn load(args: &Args) -> Result<Panel, Error> {
    let end = args.date.unwrap_or_else(|| Utc::now().date_naive());
    // 14 months of signal history plus the universe's year of history.
    let start = end - chrono::TimeDelta::days(800);
    tools::daily::stocks(&args.qdb, start, Some(end)).await
}

fn read_holdings(path: &str) -> Result<Vec<(String, u64)>, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let [sym, qty] = f.as_slice() else {
            continue;
        };
        match qty.parse::<u64>() {
            Ok(q) => out.push((sym.to_ascii_uppercase(), q)),
            Err(_) if i == 0 => {} // header
            Err(_) => return Err(format!("{path} line {}: bad share count {qty:?}", i + 1).into()),
        }
    }
    Ok(out)
}

fn lakh(x: f64) -> String {
    format!("{:.2} L", x / 1e5)
}

async fn run(args: &Args) -> Result<(), Error> {
    let ranker = build(&args.strategy, serde_json::from_str(&args.params)?)?;
    let config = RotationConfig {
        universe: UniverseConfig {
            size: args.universe_size,
            ..UniverseConfig::default()
        },
        hold: args.hold,
        exit_rank: Some(args.exit_rank),
        ..RotationConfig::recommended()
    };
    let p = load(args).await?;
    let d = match args.date {
        Some(date) => p.day_on_or_before(date),
        None => p.dates().len().checked_sub(1),
    }
    .ok_or("no data in bars_daily for that date; run dhan-daily first")?;
    let day = p.date(d).ok_or("no date")?;
    let age = (Utc::now().date_naive() - day).num_days();
    let current = match &args.holdings {
        Some(path) => read_holdings(path)?,
        None => Vec::new(),
    };
    let held: Vec<String> = current.iter().map(|(s, _)| s.clone()).collect();
    let targets = plan(&p, ranker.as_ref(), &config, d, args.capital, &held)?;
    let orders = trades(&targets, &current);

    println!(
        "{} on the close of {day} ({} stocks in the universe), capital Rs {}, {} holdings",
        ranker.name(),
        rotation::universe(&p, d, &config.universe).len(),
        lakh(args.capital),
        args.hold
    );
    if args.date.is_none() && age > 4 {
        println!(
            "WARNING: the latest data is {age} days old. Run dhan-daily to update bars_daily first."
        );
    }
    if targets.len() < args.hold {
        println!(
            "Only {} stocks pass the liquidity limit (2% of a day's traded value) at this capital.",
            targets.len()
        );
    }
    println!(
        "\n{:>4} {:<14} {:>10} {:>8} {:>12} {:>9} {:>10}",
        "rank", "symbol", "price", "shares", "value", "score", "% of day"
    );
    for t in &targets {
        println!(
            "{:>4} {:<14} {:>10.2} {:>8} {:>12} {:>9.2} {:>9.2}%",
            t.rank,
            t.symbol,
            t.price,
            t.shares,
            lakh(t.rupees),
            t.score,
            t.share_of_daily_value * 100.0
        );
    }
    let invested: f64 = targets.iter().map(|t| t.rupees).sum();
    println!(
        "\nInvested Rs {}, cash left Rs {} (whole shares only).",
        lakh(invested),
        lakh(args.capital - invested)
    );
    println!("\nOrders, sells first (at the next open; this tool places nothing):");
    if orders.is_empty() {
        println!("  none: holdings already match");
    }
    for o in &orders {
        let side = match o.side {
            Side::Buy => "BUY ",
            Side::Sell => "SELL",
        };
        println!("  {side} {:>8} {}", o.shares, o.symbol);
    }

    if let Some(path) = &args.out {
        let mut csv = String::from("kind,rank,symbol,side,shares,price,value\n");
        for t in &targets {
            writeln!(
                csv,
                "target,{},{},,{},{:.2},{:.2}",
                t.rank, t.symbol, t.shares, t.price, t.rupees
            )?;
        }
        for o in &orders {
            writeln!(csv, "order,,{},{:?},{},,", o.symbol, o.side, o.shares)?;
        }
        std::fs::write(path, csv)?;
        println!("\nWritten to {path}");
    }
    Ok(())
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn holdings_file() {
        let path = std::env::temp_dir().join("rotation_plan_holdings.csv");
        std::fs::write(&path, "symbol,shares\nreliance, 10\nTCS,3\n").unwrap();
        let h = read_holdings(path.to_str().unwrap()).unwrap();
        assert_eq!(h, vec![("RELIANCE".to_owned(), 10), ("TCS".to_owned(), 3)]);
        std::fs::write(&path, "symbol,shares\nINFY,ten\n").unwrap();
        assert!(read_holdings(path.to_str().unwrap()).is_err());
    }
}
