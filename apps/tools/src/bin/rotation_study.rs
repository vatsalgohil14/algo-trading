//! Research on monthly stock rotation, using the same `rotation` crate the
//! real rebalance plan uses: point-in-time top-N universe, equal-weight
//! holdings, next-open trades, costs on the replaced fraction.
//!
//! ```bash
//! cargo run --release -p tools --bin rotation-study                       # 2016-2026, every section
//! cargo run --release -p tools --bin rotation-study -- --from 2010-01-01 --cache data/daily_nse_2008.csv
//! cargo run --release -p tools --bin rotation-study -- --sections compare,stability
//! ```
//!
//! Sections:
//! * `compare`: every strategy at 20 and 30 holdings, against the benchmark.
//! * `stability`: the two chosen strategies across holdings, volatility
//!   penalties, momentum windows, rebalance days and rebalance frequency.
//! * `exits`: when to sell: rank buffers, fixed and trailing stops.
//! * `capacity`: the same with real capital, skipping stocks where a
//!   position would exceed 2% of a day's traded value.
//! * `sleeves`: half one strategy, half another, rebalanced together.

use std::collections::BTreeMap;
use std::process::ExitCode;

use chrono::NaiveDate;
use clap::Parser;
use rotation::{
    Backtest, FnRanker, MONTH, Momentum, MomentumConfig, MomentumLowVol, MomentumLowVolConfig,
    Panel, Ranker, RotationConfig, Schedule, Summary, UniverseConfig, backtest, excess,
};

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Clone, Parser)]
#[command(about = "Monthly rotation research on a point-in-time top-N NSE universe")]
struct Args {
    /// First trading day; data from two years earlier is used for warm-up.
    #[arg(long, default_value = "2016-01-01")]
    from: NaiveDate,
    #[arg(long, default_value = "2026-11-20")]
    to: NaiveDate,
    #[arg(long, default_value = "data/daily_nse.csv")]
    cache: String,
    #[arg(long)]
    refresh: bool,
    #[arg(long, default_value_t = 500)]
    universe_size: usize,
    #[arg(long, default_value_t = 40.0)]
    cost_bps: f64,
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "compare,stability,capacity,sleeves"
    )]
    sections: Vec<String>,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

async fn export(args: &Args) -> Result<(), Error> {
    let start = args.from - chrono::TimeDelta::days(2 * 366);
    // ts is midnight IST, so adding 5h30 gives the trading date.
    let sql = format!(
        "select to_str(dateadd('m', 330, ts), 'yyyy-MM-dd') d, symbol, open, high, close, \
         close * volume v from bars_daily where source = 'dhan_daily' and ts >= '{start}' order by ts"
    );
    let body = reqwest::Client::new()
        .get(format!("http://{}/exp", args.qdb))
        .query(&[("query", sql.as_str())])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    std::fs::write(&args.cache, body)?;
    Ok(())
}

fn high_of(p: &Panel, s: usize, d: usize, days: usize) -> Option<f64> {
    let a = d.checked_sub(days - 1)?;
    p.joined(s, a, d).then_some(())?;
    Some(
        (a..=d)
            .filter_map(|i| p.get(s, i))
            .map(|x| x.high)
            .fold(f64::MIN, f64::max),
    )
}

fn research_rankers() -> Vec<FnRanker> {
    vec![
        FnRanker {
            name: "momentum_6_1",
            score: |p, s, d| p.momentum(s, d, 6 * MONTH, MONTH),
        },
        FnRanker {
            name: "vol_adj_momentum",
            score: |p, s, d| {
                let v = p.volatility(s, d, 12 * MONTH)?;
                (v > 0.0).then_some(())?;
                Some(
                    (p.momentum(s, d, 12 * MONTH, MONTH)? + p.momentum(s, d, 6 * MONTH, MONTH)?)
                        / v
                        / 2.0,
                )
            },
        },
        FnRanker {
            name: "low_volatility",
            score: |p, s, d| Some(-p.volatility(s, d, 12 * MONTH)?),
        },
        FnRanker {
            name: "near_52w_high",
            score: |p, s, d| Some(p.get(s, d)?.close / high_of(p, s, d, 12 * MONTH)?),
        },
        FnRanker {
            name: "far_below_6m_high",
            score: |p, s, d| Some(-(p.get(s, d)?.close / high_of(p, s, d, 6 * MONTH)?)),
        },
        FnRanker {
            name: "reversal_1m",
            score: |p, s, d| Some(-p.ret(s, d.checked_sub(MONTH)?, d)?),
        },
        FnRanker {
            name: "momentum_consistency",
            // Months up out of the last 11 (skipping the latest), with 12-1
            // momentum breaking ties: steady winners, not one jump.
            score: |p, s, d| {
                let mut up = 0.0;
                for m in 1..12 {
                    let r = p.ret(
                        s,
                        d.checked_sub((m + 1) * MONTH)?,
                        d.checked_sub(m * MONTH)?,
                    )?;
                    if r > 0.0 {
                        up += 1.0;
                    }
                }
                Some(up + p.momentum(s, d, 12 * MONTH, MONTH)?.clamp(-0.99, 0.99))
            },
        },
    ]
}

/// Momentum of the part of returns the market doesn't explain: over the
/// 12 months before the last one, the mean residual return after removing
/// beta × the market's, divided by its volatility. Known to crash less than
/// plain momentum.
#[derive(Debug)]
struct ResidualMomentum {
    market: Vec<f64>,
}

impl Ranker for ResidualMomentum {
    fn name(&self) -> &str {
        "residual_momentum"
    }

    fn score(&self, p: &Panel, s: usize, d: usize) -> Option<f64> {
        let end = d.checked_sub(MONTH)?;
        let k = 11 * MONTH;
        let r = p.daily_returns(s, end, k)?;
        let start = end.checked_sub(k)? + 1;
        let m: Vec<f64> = (start..=end)
            .map(|i| self.market.get(i).copied())
            .collect::<Option<_>>()?;
        let n = r.len().min(m.len()) as f64;
        (n > 20.0).then_some(())?;
        let (mr, mm) = (r.iter().sum::<f64>() / n, m.iter().sum::<f64>() / n);
        let cov: f64 = r.iter().zip(&m).map(|(a, b)| (a - mr) * (b - mm)).sum();
        let var: f64 = m.iter().map(|b| (b - mm).powi(2)).sum();
        (var > 0.0).then_some(())?;
        let beta = cov / var;
        let resid: Vec<f64> = r.iter().zip(&m).map(|(a, b)| a - beta * b).collect();
        let mean = resid.iter().sum::<f64>() / n;
        let sd = (resid.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
        (sd > 0.0).then(|| mean / sd)
    }
}

/// Daily equal-weight return of the universe, re-picked monthly from data
/// before each day.
fn market_returns(p: &Panel, c: &UniverseConfig) -> Vec<f64> {
    let mut out = vec![0.0; p.dates().len()];
    let mut members: Vec<usize> = Vec::new();
    for d in 1..p.dates().len() {
        if d % MONTH == 0 || members.is_empty() {
            let u = rotation::universe(p, d - 1, c);
            if !u.is_empty() {
                members = u;
            }
        }
        let rs: Vec<f64> = members.iter().filter_map(|&s| p.ret(s, d - 1, d)).collect();
        if let Some(x) = out.get_mut(d) {
            *x = if rs.is_empty() {
                0.0
            } else {
                rs.iter().sum::<f64>() / rs.len() as f64
            };
        }
    }
    out
}

fn pct(x: f64) -> String {
    format!("{:+.1}%", x * 100.0)
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
        "{label:<36} {:>7} {:>6.1}% {:>5.2} {:>7} {:>7} {:>5.1}x  {ex}   {}",
        pct(s.cagr),
        s.volatility * 100.0,
        s.sharpe,
        pct(-s.max_drawdown),
        pct(s.worst_year),
        s.turnover_per_year,
        years.join(" ")
    );
}

fn header(title: &str) {
    println!("\n=== {title}");
    println!(
        "{:<36} {:>7} {:>7} {:>5} {:>7} {:>7} {:>6}  {:>7} {:>6}   by year",
        "", "CAGR", "vol", "Shrp", "max DD", "worst", "turns", "excess", "t"
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
}

fn with(base: &RotationConfig, f: impl FnOnce(&mut RotationConfig)) -> RotationConfig {
    let mut c = base.clone();
    f(&mut c);
    c
}

async fn run(args: &Args) -> Result<(), Error> {
    if args.refresh || std::fs::metadata(&args.cache).is_err() {
        export(args).await?;
    }
    let p = Panel::from_csv(&std::fs::read_to_string(&args.cache)?)?;
    let base = RotationConfig {
        universe: UniverseConfig {
            size: args.universe_size,
            ..UniverseConfig::default()
        },
        cost_bps: args.cost_bps,
        ..RotationConfig::default()
    };
    let ctx = Ctx {
        p: &p,
        from: args.from,
        to: args.to,
    };
    println!(
        "{} stocks, {} days; trading {} to {}; universe top {} by median traded value; {} bps per round trip",
        p.stocks(),
        p.dates().len(),
        args.from,
        args.to,
        args.universe_size,
        args.cost_bps
    );
    println!(
        "excess = average net gain per rebalance over the equal-weight universe; t above 3 = strong"
    );

    let ml = MomentumLowVol::new(MomentumLowVolConfig::default())?;
    let m12 = Momentum::new(MomentumConfig::default())?;
    let residual = ResidualMomentum {
        market: market_returns(&p, &base.universe),
    };
    let wants = |s: &str| args.sections.iter().any(|x| x == s);

    if wants("compare") {
        header("compare: all strategies");
        let bench = ctx.run(None, &base, None)?;
        row(
            &format!("benchmark: all {}", args.universe_size),
            &bench.summary(),
            None,
        );
        let research = research_rankers();
        let mut all: Vec<&dyn Ranker> = vec![&m12, &ml, &residual];
        all.extend(research.iter().map(|r| r as &dyn Ranker));
        for r in all {
            for hold in [20, 30] {
                ctx.show(
                    &format!("{} top {hold}", r.name()),
                    r,
                    &with(&base, |c| c.hold = hold),
                    None,
                )?;
            }
        }
    }

    if wants("stability") {
        header("stability: number of holdings");
        for hold in [10, 15, 20, 30, 40, 50] {
            let c = with(&base, |c| c.hold = hold);
            ctx.show(&format!("momentum_low_vol top {hold}"), &ml, &c, None)?;
            ctx.show(&format!("momentum_12_1 top {hold}"), &m12, &c, None)?;
        }
        header("stability: momentum_low_vol volatility penalty (top 30)");
        for pen in [0.0, 1.0, 2.0, 3.0, 4.0, 6.0] {
            let r = MomentumLowVol::new(MomentumLowVolConfig { vol_penalty: pen })?;
            ctx.show(&format!("vol_penalty {pen}"), &r, &base, None)?;
        }
        header("stability: momentum windows, months lookback-skip (top 30)");
        for (l, s) in [
            (12, 1),
            (12, 2),
            (12, 3),
            (9, 1),
            (6, 1),
            (3, 1),
            (18, 1),
            (24, 1),
        ] {
            let r = Momentum::new(MomentumConfig {
                lookback_months: l,
                skip_months: s,
            })?;
            ctx.show(&format!("momentum {l}-{s}"), &r, &base, None)?;
        }
        header("stability: rebalance day, every 21 trading days from shifted starts (top 30)");
        let every21 = with(&base, |c| c.schedule = Schedule::EveryDays(MONTH));
        for shift in [0, 5, 10, 15] {
            let Some(from) = p.day_on_or_after(args.from).and_then(|i| p.date(i + shift)) else {
                continue;
            };
            let shifted = Ctx {
                p: &p,
                from,
                to: args.to,
            };
            shifted.show(
                &format!("momentum_low_vol from {from}"),
                &ml,
                &every21,
                None,
            )?;
            shifted.show(&format!("momentum_12_1 from {from}"), &m12, &every21, None)?;
        }
        header("stability: rebalance frequency (top 30)");
        for (label, days) in [
            ("every 2 weeks", Schedule::EveryDays(10)),
            ("every 21 trading days", Schedule::EveryDays(21)),
            ("first trading day of the month", Schedule::MonthStart),
            ("every 2 months", Schedule::EveryDays(42)),
            ("quarterly", Schedule::EveryDays(63)),
        ] {
            let c = with(&base, |c| c.schedule = days);
            ctx.show(&format!("momentum_low_vol {label}"), &ml, &c, None)?;
            ctx.show(&format!("momentum_12_1 {label}"), &m12, &c, None)?;
        }
    }

    if wants("exits") {
        header("exits: when a held stock is sold (top 30, monthly)");
        type Exit = (&'static str, Option<usize>, Option<f64>, Option<f64>);
        let exits: [Exit; 8] = [
            ("leaves the top 30 (default)", None, None, None),
            ("rank buffer: leaves the top 45", Some(45), None, None),
            ("rank buffer: leaves the top 60", Some(60), None, None),
            ("rank buffer: leaves the top 90", Some(90), None, None),
            ("fixed stop 15% below entry", None, Some(0.85), None),
            ("fixed stop 25% below entry", None, Some(0.75), None),
            ("trailing stop 20% below peak", None, None, Some(0.80)),
            ("buffer 60 + trailing stop 25%", Some(60), None, Some(0.75)),
        ];
        for (label, exit_rank, stop_loss, trailing_stop) in exits {
            let c = with(&base, |c| {
                c.exit_rank = exit_rank;
                c.stop_loss = stop_loss;
                c.trailing_stop = trailing_stop;
            });
            ctx.show(&format!("low_vol: {label}"), &ml, &c, None)?;
            ctx.show(&format!("12-1:    {label}"), &m12, &c, None)?;
        }
    }

    if wants("capacity") {
        header("capacity: capital, skipping positions above 2% of a day's traded value (top 30)");
        for cap in [1e6, 5e6, 2e7, 1e8, 5e8] {
            let label = |name: &str| format!("{name} Rs {:.0} lakh", cap / 1e5);
            ctx.show(&label("momentum_low_vol"), &ml, &base, Some(cap))?;
            ctx.show(&label("momentum_12_1"), &m12, &base, Some(cap))?;
        }
    }

    if wants("sleeves") {
        header("sleeves: half one strategy, half another (15 stocks each), rebalanced monthly");
        let low = FnRanker {
            name: "low_volatility",
            score: |p, s, d| Some(-p.volatility(s, d, 12 * MONTH)?),
        };
        let c15 = with(&base, |c| c.hold = 15);
        let bench = ctx.run(None, &base, None)?;
        let pairs: [(&str, &dyn Ranker, &dyn Ranker); 3] = [
            ("momentum_12_1 + low_volatility", &m12, &low),
            ("momentum_low_vol + low_volatility", &ml, &low),
            ("momentum_12_1 + residual_momentum", &m12, &residual),
        ];
        for (label, a, b) in pairs {
            let (x, y) = (ctx.run(Some(a), &c15, None)?, ctx.run(Some(b), &c15, None)?);
            let ynet: BTreeMap<NaiveDate, f64> = y.net().into_iter().collect();
            let mut combined = x.clone();
            for (period, (_, xnet)) in combined.periods.iter_mut().zip(x.net()) {
                period.gross = (xnet + ynet.get(&period.entry).copied().unwrap_or(0.0)) / 2.0;
            }
            // Costs are already inside each half's net return.
            combined.cost_bps = 0.0;
            row(label, &combined.summary(), Some(excess(&combined, &bench)));
        }
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
