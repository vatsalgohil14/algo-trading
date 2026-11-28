//! Backtests of the two rotation strategies, `momentum_low_vol` and
//! `momentum` (12-1), with the same `rotation` crate the paper account and
//! the monthly plan use: point-in-time top-500 universe, equal weights,
//! next-open trades, costs on the replaced fraction.
//!
//! ```bash
//! scripts/questdb.sh start
//! cargo run --release -p tools --bin rotation-study                      # 2010-2026, every section
//! cargo run --release -p tools --bin rotation-study -- --sections compare,exits
//! cargo run --release -p tools --bin rotation-study -- --from 2016-01-01 --cache data/daily_nse_2014.csv
//! ```
//!
//! The first run exports `bars_daily` to the cache file (from two years
//! before `--from`, for the signals' warm-up); later runs read the cache.
//! `--refresh` exports again.
//!
//! Sections:
//! * `compare`: both strategies at 20 and 30 holdings, against the
//!   equal-weight universe.
//! * `stability`: holdings, volatility penalty, momentum windows, rebalance
//!   day and frequency.
//! * `exits`: the rank buffer (sell below 30, 45, 60 or 90).
//! * `capacity`: real capital, skipping positions above 2% of a day's
//!   traded value.
//! * `costs`: doubled costs.

use std::process::ExitCode;

use chrono::NaiveDate;
use clap::Parser;
use rotation::{
    Backtest, MONTH, Momentum, MomentumConfig, MomentumLowVol, MomentumLowVolConfig, Panel, Ranker,
    RotationConfig, Schedule, Summary, UniverseConfig, backtest, excess,
};

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Clone, Parser)]
#[command(about = "Backtests of the momentum rotation strategies")]
struct Args {
    /// First trading day; data from two years earlier is used for warm-up.
    #[arg(long, default_value = "2010-01-01")]
    from: NaiveDate,
    /// Last day. Default: the end of the data.
    #[arg(long)]
    to: Option<NaiveDate>,
    #[arg(long, default_value = "data/daily_nse_2008.csv")]
    cache: String,
    /// Export bars_daily to the cache again.
    #[arg(long)]
    refresh: bool,
    #[arg(long, default_value_t = 500)]
    universe_size: usize,
    #[arg(long, default_value_t = 40.0)]
    cost_bps: f64,
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "compare,stability,exits,capacity,costs"
    )]
    sections: Vec<String>,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

fn pct(x: f64) -> String {
    format!("{:+.1}%", x * 100.0)
}

fn header(title: &str) {
    println!("\n=== {title}");
    println!(
        "{:<40} {:>7} {:>7} {:>5} {:>7} {:>7} {:>6}  {:>7} {:>6}   by year",
        "", "CAGR", "vol", "Shrp", "max DD", "worst", "turns", "excess", "t"
    );
}

fn row(label: &str, s: &Summary, ex: Option<(f64, f64)>) {
    let years: Vec<String> = s
        .by_year
        .iter()
        .map(|(y, r)| format!("{}:{:+.0}", y % 100, r * 100.0))
        .collect();
    let ex = ex.map_or(String::from("       -      -"), |(m, t)| {
        format!("{:>+6.2}% {:>6.1}", m * 100.0, t)
    });
    println!(
        "{label:<40} {:>7} {:>6.1}% {:>5.2} {:>7} {:>7} {:>5.1}x  {ex}   {}",
        pct(s.cagr),
        s.volatility * 100.0,
        s.sharpe,
        pct(-s.max_drawdown),
        pct(s.worst_year),
        s.turnover_per_year,
        years.join(" ")
    );
}

struct Ctx<'a> {
    p: &'a Panel,
    from: NaiveDate,
    to: NaiveDate,
}

impl Ctx<'_> {
    fn run(
        &self,
        r: Option<&dyn Ranker>,
        c: &RotationConfig,
        capital: Option<f64>,
    ) -> Result<Backtest, Error> {
        Ok(backtest(self.p, r, c, self.from, self.to, capital)?)
    }

    /// Runs `r` with `c` and prints it against the benchmark on the same
    /// rebalance days.
    fn show(
        &self,
        label: &str,
        r: &dyn Ranker,
        c: &RotationConfig,
        capital: Option<f64>,
    ) -> Result<(), Error> {
        let bt = self.run(Some(r), c, capital)?;
        let bench = self.run(None, c, None)?;
        row(label, &bt.summary(), Some(excess(&bt, &bench)));
        Ok(())
    }

    /// Both strategies with the same settings.
    fn both(&self, label: &str, rs: &[&dyn Ranker], c: &RotationConfig) -> Result<(), Error> {
        for r in rs {
            self.show(&format!("{} {label}", r.name()), *r, c, None)?;
        }
        Ok(())
    }
}

fn with(base: &RotationConfig, f: impl FnOnce(&mut RotationConfig)) -> RotationConfig {
    let mut c = base.clone();
    f(&mut c);
    c
}

async fn load(args: &Args) -> Result<Panel, Error> {
    if args.refresh || std::fs::metadata(&args.cache).is_err() {
        let start = args.from - chrono::TimeDelta::days(2 * 366);
        println!("Exporting bars_daily from {start} to {} ...", args.cache);
        let p = tools::daily::stocks_csv(&args.qdb, start, None).await?;
        std::fs::write(&args.cache, p)?;
    }
    Ok(Panel::from_csv(&std::fs::read_to_string(&args.cache)?)?)
}

async fn run(args: &Args) -> Result<(), Error> {
    let p = load(args).await?;
    let to = args
        .to
        .or_else(|| p.dates().last().copied())
        .ok_or("no data")?;
    let base = RotationConfig {
        universe: UniverseConfig {
            size: args.universe_size,
            ..UniverseConfig::default()
        },
        cost_bps: args.cost_bps,
        ..RotationConfig::recommended()
    };
    let ctx = Ctx {
        p: &p,
        from: args.from,
        to,
    };
    println!(
        "{} stocks, {} days; trading {} to {to}; universe top {} by median traded value; \
         {} bps per round trip; hold {}, exit rank {}, first trading day of the month",
        p.stocks(),
        p.dates().len(),
        args.from,
        args.universe_size,
        args.cost_bps,
        base.hold,
        base.exit_rank.unwrap_or(base.hold),
    );
    println!(
        "excess = average net gain per rebalance over the equal-weight universe; t above 3 = strong"
    );

    let ml = MomentumLowVol::new(MomentumLowVolConfig::default())?;
    let m12 = Momentum::new(MomentumConfig::default())?;
    let both: [&dyn Ranker; 2] = [&ml, &m12];
    let wants = |s: &str| args.sections.iter().any(|x| x == s);

    if wants("compare") {
        header("compare: the two strategies against the market");
        let bench = ctx.run(None, &base, None)?;
        row(
            &format!("benchmark: all {}, equal weight", args.universe_size),
            &bench.summary(),
            None,
        );
        for hold in [20, 30] {
            let c = with(&base, |c| {
                c.hold = hold;
                c.exit_rank = Some(hold * 2);
            });
            ctx.both(&format!("top {hold}, exit rank {}", hold * 2), &both, &c)?;
        }
    }

    if wants("stability") {
        header("stability: number of holdings (exit rank 2x holdings)");
        for hold in [10, 15, 20, 30, 40, 50] {
            let c = with(&base, |c| {
                c.hold = hold;
                c.exit_rank = Some(hold * 2);
            });
            ctx.both(&format!("top {hold}"), &both, &c)?;
        }
        header("stability: momentum_low_vol volatility penalty");
        for pen in [0.0, 1.0, 2.0, 3.0, 4.0, 6.0] {
            let r = MomentumLowVol::new(MomentumLowVolConfig { vol_penalty: pen })?;
            ctx.show(&format!("vol_penalty {pen}"), &r, &base, None)?;
        }
        header("stability: momentum windows, months lookback-skip");
        for (l, s) in [(12, 1), (12, 2), (12, 3), (9, 1), (6, 1), (18, 1)] {
            let r = Momentum::new(MomentumConfig {
                lookback_months: l,
                skip_months: s,
            })?;
            ctx.show(&format!("momentum {l}-{s}"), &r, &base, None)?;
        }
        header("stability: rebalance day, every 21 trading days from shifted starts");
        let every21 = with(&base, |c| c.schedule = Schedule::EveryDays(MONTH));
        for shift in [0, 5, 10, 15] {
            let Some(from) = p.day_on_or_after(args.from).and_then(|i| p.date(i + shift)) else {
                continue;
            };
            let shifted = Ctx { p: &p, from, to };
            shifted.both(&format!("from {from}"), &both, &every21)?;
        }
        header("stability: rebalance frequency");
        for (label, schedule) in [
            ("every 2 weeks", Schedule::EveryDays(10)),
            ("first trading day of the month", Schedule::MonthStart),
            ("every 2 months", Schedule::EveryDays(42)),
            ("quarterly", Schedule::EveryDays(63)),
        ] {
            ctx.both(label, &both, &with(&base, |c| c.schedule = schedule))?;
        }
    }

    if wants("exits") {
        header("exits: a held stock is sold when it ranks below ... (top 30)");
        for exit in [30, 45, 60, 90] {
            let c = with(&base, |c| c.exit_rank = Some(exit));
            ctx.both(&format!("rank {exit}"), &both, &c)?;
        }
    }

    if wants("capacity") {
        header("capacity: skipping positions above 2% of a day's traded value");
        for cap in [1e6, 5e6, 2e7, 1e8, 5e8] {
            for r in both {
                ctx.show(
                    &format!("{} Rs {:.0} lakh", r.name(), cap / 1e5),
                    r,
                    &base,
                    Some(cap),
                )?;
            }
        }
    }

    if wants("costs") {
        header("costs: doubled to 80 bps per round trip");
        ctx.both("80 bps", &both, &with(&base, |c| c.cost_bps = 80.0))?;
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
