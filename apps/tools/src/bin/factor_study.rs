//! Factor study: rank the universe each day on signals built from daily
//! bars, and measure whether the top of the ranking beats the bottom over
//! the following days, after costs.
//!
//! Intraday single-stock signals move a few basis points against 15 bps of
//! costs (see docs/research.md). Holding for days lets a move grow past the
//! costs, and ranking stocks against each other removes most of the
//! market's own swings.
//!
//! ```bash
//! cargo run --release -p tools --bin factor-study            # builds the daily cache once
//! cargo run --release -p tools --bin factor-study -- --refresh
//! ```
//!
//! Every signal is known at the close of day `d`. Trades enter at the open
//! of `d + 1` and exit at the open of `d + 1 + h`, so nothing uses a price
//! that was not yet printed.
//!
//! The combined model is walk-forward: on each day it weights the signals
//! by how well each predicted returns over the past only (returns whose
//! holding period had already ended), so its results are out of sample.

// Research tool over fixed-size matrices built in this file: every index is
// bounded by construction, and a panic here is a bug to see, not a trading
// risk.
#![allow(clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::process::ExitCode;

use backtest::{BarSource, QuestDbSource, load_universe_file};
use chrono::{Datelike, NaiveDate};
use clap::Parser;
use trading_core::Bar;

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Parser)]
#[command(about = "Cross-sectional daily factor study with a walk-forward combined model")]
struct Args {
    #[arg(long, default_value = "data/universe.csv")]
    universe: String,
    #[arg(long, default_value = "2021-12-01")]
    from: NaiveDate,
    #[arg(long, default_value = "2026-11-20")]
    to: NaiveDate,
    /// Daily bars built from the 1-minute history, cached here.
    #[arg(long, default_value = "data/daily_bars.csv")]
    cache: String,
    /// Rebuild the daily cache from QuestDB.
    #[arg(long)]
    refresh: bool,
    /// Holding periods in trading days.
    #[arg(long, value_delimiter = ',', default_value = "1,5,10,20")]
    horizons: Vec<usize>,
    /// Stocks held on each side.
    #[arg(long, default_value_t = 10)]
    top: usize,
    /// Round trip for cash delivery, bps: STT 0.1% each side, stamp duty,
    /// exchange and DP charges, plus slippage.
    #[arg(long, default_value_t = 28.0)]
    delivery_bps: f64,
    /// Round trip for stock futures, bps: STT 0.05% on the sell side
    /// (from April 2026), exchange charges, brokerage, plus slippage.
    #[arg(long, default_value_t = 10.0)]
    futures_bps: f64,
    /// Days of history before the combined model starts trading.
    #[arg(long, default_value_t = 250)]
    warmup: usize,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

/// One stock's day.
#[derive(Debug, Clone, Copy)]
struct Day {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    vwap: f64,
}

/// A day-to-day jump this large is an unadjusted split, bonus or demerger,
/// not trading: the stock's history restarts after it.
const BREAK_PCT: f64 = 15.0;
/// Cross-sections with fewer stocks than this are skipped.
const MIN_STOCKS: usize = 20;
const TRADING_DAYS: f64 = 247.0;

/// Daily bars for every stock, aligned to the union of trading dates.
struct Panel {
    symbols: Vec<String>,
    dates: Vec<NaiveDate>,
    /// `days[stock][date]`.
    days: Vec<Vec<Option<Day>>>,
    /// Stretch id: two dates in the same stretch have every day between
    /// them present, with no break.
    stretch: Vec<Vec<u32>>,
}

impl Panel {
    fn day(&self, s: usize, d: usize) -> Option<Day> {
        self.days[s][d]
    }

    /// Both dates present and joined by an unbroken run of days.
    fn joined(&self, s: usize, a: usize, b: usize) -> bool {
        self.days[s][a].is_some()
            && self.days[s][b].is_some()
            && self.stretch[s][a] == self.stretch[s][b]
    }

    fn build(symbols: Vec<String>, rows: BTreeMap<NaiveDate, Vec<Option<Day>>>) -> Self {
        let n = symbols.len();
        let dates: Vec<NaiveDate> = rows.keys().copied().collect();
        let mut days = vec![Vec::with_capacity(dates.len()); n];
        for row in rows.values() {
            for (s, d) in row.iter().enumerate() {
                days[s].push(*d);
            }
        }
        let mut stretch = vec![vec![0; dates.len()]; n];
        for s in 0..n {
            let mut id = 0;
            let mut prev: Option<Day> = None;
            for d in 0..dates.len() {
                match (prev, days[s][d]) {
                    (Some(p), Some(today)) => {
                        let jump = (today.open / p.close - 1.0).abs() * 100.0;
                        if jump > BREAK_PCT {
                            id += 1;
                        }
                    }
                    (None, Some(_)) => id += 1,
                    _ => {}
                }
                stretch[s][d] = id;
                prev = days[s][d];
            }
        }
        Self {
            symbols,
            dates,
            days,
            stretch,
        }
    }

    /// Where the history restarts, for the report.
    fn breaks(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in 0..self.symbols.len() {
            for d in 1..self.dates.len() {
                if let (Some(p), Some(t)) = (self.days[s][d - 1], self.days[s][d]) {
                    let jump = (t.open / p.close - 1.0) * 100.0;
                    if jump.abs() > BREAK_PCT {
                        out.push(format!(
                            "{} {} {:+.0}%",
                            self.symbols[s], self.dates[d], jump
                        ));
                    }
                }
            }
        }
        out
    }
}

fn aggregate(bars: &[Bar]) -> Option<Day> {
    let first = bars.first()?;
    let last = bars.last()?;
    let mut day = Day {
        open: first.open.as_f64(),
        high: f64::MIN,
        low: f64::MAX,
        close: last.close.as_f64(),
        volume: 0.0,
        vwap: 0.0,
    };
    let mut value = 0.0;
    for b in bars {
        day.high = day.high.max(b.high.as_f64());
        day.low = day.low.min(b.low.as_f64());
        let v = b.volume as f64;
        day.volume += v;
        value += v * (b.high.as_f64() + b.low.as_f64() + b.close.as_f64()) / 3.0;
    }
    day.vwap = if day.volume > 0.0 {
        value / day.volume
    } else {
        day.close
    };
    (day.open > 0.0 && day.close > 0.0).then_some(day)
}

/// Builds daily bars from QuestDB and writes the cache.
fn build_cache(args: &Args) -> Result<(), Error> {
    let (universe, ids) = load_universe_file(&args.universe)?;
    let n = universe.len();
    let mut source = QuestDbSource::connect(&args.qdb, ids, args.from, args.to)?;
    let mut out = String::from("date,symbol,open,high,low,close,volume,vwap\n");
    let mut sessions = 0;
    while let Some((date, bars)) = source.next_session()? {
        let mut per: Vec<Vec<Bar>> = vec![Vec::new(); n];
        for b in bars {
            if let Some(v) = per.get_mut(b.instrument.index()) {
                v.push(b);
            }
        }
        for (i, bars) in per.iter().enumerate() {
            // A day with under an hour of candles is a data gap, not a session.
            if bars.len() < 60 {
                continue;
            }
            let (Some(day), Some(inst)) = (
                aggregate(bars),
                universe.get(trading_core::InstrumentId(i as u32)),
            ) else {
                continue;
            };
            writeln!(
                out,
                "{date},{},{},{},{},{},{},{:.4}",
                inst.symbol, day.open, day.high, day.low, day.close, day.volume, day.vwap
            )?;
        }
        sessions += 1;
    }
    fs::write(&args.cache, out)?;
    eprintln!("cached {sessions} sessions to {}", args.cache);
    Ok(())
}

fn load_cache(path: &str) -> Result<Panel, Error> {
    let text = fs::read_to_string(path)?;
    let mut symbols: Vec<String> = Vec::new();
    let mut raw: Vec<(NaiveDate, usize, Day)> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 8 {
            continue;
        }
        let date: NaiveDate = f[0].parse()?;
        let s = match symbols.iter().position(|x| x == f[1]) {
            Some(s) => s,
            None => {
                symbols.push(f[1].to_owned());
                symbols.len() - 1
            }
        };
        let num = |i: usize| f[i].parse::<f64>();
        raw.push((
            date,
            s,
            Day {
                open: num(2)?,
                high: num(3)?,
                low: num(4)?,
                close: num(5)?,
                volume: num(6)?,
                vwap: num(7)?,
            },
        ));
    }
    let mut rows: BTreeMap<NaiveDate, Vec<Option<Day>>> = BTreeMap::new();
    for (date, s, day) in raw {
        let row = rows
            .entry(date)
            .or_insert_with(|| vec![None; symbols.len()]);
        row.resize(symbols.len(), None);
        row[s] = Some(day);
    }
    for row in rows.values_mut() {
        row.resize(symbols.len(), None);
    }
    Ok(Panel::build(symbols, rows))
}

/// A signal: higher means "expected to do better". `None` when the stock
/// lacks the history.
struct Factor {
    name: &'static str,
    about: &'static str,
    calc: fn(&Panel, usize, usize) -> Option<f64>,
}

fn ret(p: &Panel, s: usize, a: usize, b: usize) -> Option<f64> {
    (b >= a && p.joined(s, a, b)).then_some(())?;
    Some(p.day(s, b)?.close / p.day(s, a)?.close - 1.0)
}

fn back(d: usize, k: usize) -> Option<usize> {
    d.checked_sub(k)
}

fn daily_returns(p: &Panel, s: usize, d: usize, k: usize) -> Option<Vec<f64>> {
    let a = back(d, k)?;
    p.joined(s, a, d).then_some(())?;
    (a + 1..=d)
        .map(|i| Some(p.day(s, i)?.close / p.day(s, i - 1)?.close - 1.0))
        .collect()
}

fn factors() -> Vec<Factor> {
    vec![
        Factor {
            name: "rev_1d",
            about: "minus today's return: yesterday's losers bounce",
            calc: |p, s, d| Some(-ret(p, s, back(d, 1)?, d)?),
        },
        Factor {
            name: "rev_5d",
            about: "minus the last week's return",
            calc: |p, s, d| Some(-ret(p, s, back(d, 5)?, d)?),
        },
        Factor {
            name: "mom_1m",
            about: "return over the month before last week",
            calc: |p, s, d| ret(p, s, back(d, 25)?, back(d, 5)?),
        },
        Factor {
            name: "mom_3m",
            about: "return over 3 months, skipping last week",
            calc: |p, s, d| ret(p, s, back(d, 65)?, back(d, 5)?),
        },
        Factor {
            name: "mom_6m",
            about: "return over 6 months, skipping last week",
            calc: |p, s, d| ret(p, s, back(d, 125)?, back(d, 5)?),
        },
        Factor {
            name: "intraday_rev",
            about: "minus today's open-to-close move",
            calc: |p, s, d| {
                let x = p.day(s, d)?;
                Some(-(x.close / x.open - 1.0))
            },
        },
        Factor {
            name: "gap",
            about: "today's opening gap",
            calc: |p, s, d| {
                let a = back(d, 1)?;
                p.joined(s, a, d).then_some(())?;
                Some(p.day(s, d)?.open / p.day(s, a)?.close - 1.0)
            },
        },
        Factor {
            name: "low_vol",
            about: "minus 20-day volatility of daily returns",
            calc: |p, s, d| {
                let r = daily_returns(p, s, d, 20)?;
                let m = r.iter().sum::<f64>() / r.len() as f64;
                Some(-(r.iter().map(|x| (x - m).powi(2)).sum::<f64>() / r.len() as f64).sqrt())
            },
        },
        Factor {
            name: "vol_surge",
            about: "log of today's volume over its 20-day average",
            calc: |p, s, d| {
                let a = back(d, 20)?;
                p.joined(s, a, d).then_some(())?;
                let avg = (a..d)
                    .map(|i| p.day(s, i).map_or(0.0, |x| x.volume))
                    .sum::<f64>()
                    / 20.0;
                let v = p.day(s, d)?.volume;
                (avg > 0.0 && v > 0.0).then(|| (v / avg).ln())
            },
        },
        Factor {
            name: "close_loc",
            about: "where today closed in its range (1 = high)",
            calc: |p, s, d| {
                let x = p.day(s, d)?;
                (x.high > x.low).then(|| (x.close - x.low) / (x.high - x.low))
            },
        },
        Factor {
            name: "near_high",
            about: "close over the 6-month high (0 = at the high)",
            calc: |p, s, d| {
                let a = back(d, 125)?;
                p.joined(s, a, d).then_some(())?;
                let hi = (a..=d)
                    .filter_map(|i| p.day(s, i))
                    .map(|x| x.high)
                    .fold(f64::MIN, f64::max);
                Some(p.day(s, d)?.close / hi - 1.0)
            },
        },
        Factor {
            name: "vs_vwap",
            about: "close relative to today's VWAP",
            calc: |p, s, d| {
                let x = p.day(s, d)?;
                Some(x.close / x.vwap - 1.0)
            },
        },
        Factor {
            name: "overnight_1m",
            about: "sum of overnight returns (close to open) over 20 days",
            calc: |p, s, d| {
                let a = back(d, 20)?;
                p.joined(s, a, d).then_some(())?;
                (a + 1..=d)
                    .map(|i| Some(p.day(s, i)?.open / p.day(s, i - 1)?.close - 1.0))
                    .sum()
            },
        },
        Factor {
            name: "intraday_1m",
            about: "sum of open-to-close returns over 20 days",
            calc: |p, s, d| {
                let a = back(d, 19)?;
                p.joined(s, a, d).then_some(())?;
                (a..=d)
                    .map(|i| p.day(s, i).map(|x| x.close / x.open - 1.0))
                    .sum()
            },
        },
    ]
}

/// Ranks scaled to -0.5..0.5 for the present values; ties share a rank.
fn ranks(xs: &[Option<f64>]) -> Vec<Option<f64>> {
    let mut idx: Vec<usize> = (0..xs.len()).filter(|&i| xs[i].is_some()).collect();
    let n = idx.len();
    let mut out = vec![None; xs.len()];
    if n < 2 {
        return out;
    }
    idx.sort_by(|&a, &b| xs[a].unwrap_or(0.0).total_cmp(&xs[b].unwrap_or(0.0)));
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && xs[idx[j + 1]] == xs[idx[i]] {
            j += 1;
        }
        let r = (i + j) as f64 / 2.0 / (n - 1) as f64 - 0.5;
        for &k in &idx[i..=j] {
            out[k] = Some(r);
        }
        i = j + 1;
    }
    out
}

/// Spearman correlation of two cross-sections.
fn rank_ic(a: &[Option<f64>], b: &[Option<f64>]) -> Option<f64> {
    let both: Vec<(f64, f64)> = a
        .iter()
        .zip(b)
        .filter_map(|(x, y)| Some(((*x)?, (*y)?)))
        .collect();
    if both.len() < MIN_STOCKS {
        return None;
    }
    let ra = ranks(&both.iter().map(|x| Some(x.0)).collect::<Vec<_>>());
    let rb = ranks(&both.iter().map(|x| Some(x.1)).collect::<Vec<_>>());
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in ra.iter().zip(&rb) {
        let (x, y) = ((*x)?, (*y)?);
        sxy += x * y;
        sxx += x * x;
        syy += y * y;
    }
    (sxx > 0.0 && syy > 0.0).then(|| sxy / (sxx * syy).sqrt())
}

fn mean_t(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    if xs.len() < 2 {
        return (xs.first().copied().unwrap_or(0.0), 0.0);
    }
    let m = xs.iter().sum::<f64>() / n;
    let sd = (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    (m, if sd > 0.0 { m / sd * n.sqrt() } else { 0.0 })
}

/// Forward return from the open of `d + 1` to the open of `d + 1 + h`.
fn forward(p: &Panel, s: usize, d: usize, h: usize) -> Option<f64> {
    let (a, b) = (d + 1, d + 1 + h);
    (b < p.dates.len() && p.joined(s, a, b)).then_some(())?;
    Some(p.day(s, b)?.open / p.day(s, a)?.open - 1.0)
}

/// Results of trading one signal every `h` days.
#[derive(Default)]
struct Book {
    /// Per rebalance: (date, long-only net, long-short net, long excess over
    /// the universe before costs, universe average).
    periods: Vec<(NaiveDate, f64, f64, f64, f64)>,
    turnover: Vec<f64>,
}

struct Summary {
    cagr: f64,
    sharpe: f64,
    max_dd: f64,
    by_year: BTreeMap<i32, f64>,
}

fn summarize(rets: &[(NaiveDate, f64)], h: usize) -> Summary {
    let per_year = TRADING_DAYS / h as f64;
    let mut eq = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut max_dd = 0.0_f64;
    let mut by_year: BTreeMap<i32, f64> = BTreeMap::new();
    for &(date, r) in rets {
        eq *= 1.0 + r;
        peak = peak.max(eq);
        max_dd = max_dd.max(1.0 - eq / peak);
        let y = by_year.entry(date.year()).or_insert(1.0);
        *y *= 1.0 + r;
    }
    for v in by_year.values_mut() {
        *v -= 1.0;
    }
    let xs: Vec<f64> = rets.iter().map(|x| x.1).collect();
    let n = xs.len() as f64;
    let m = xs.iter().sum::<f64>() / n.max(1.0);
    let sd = (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
    let years = n / per_year;
    Summary {
        cagr: if years > 0.0 && eq > 0.0 {
            eq.powf(1.0 / years) - 1.0
        } else {
            -1.0
        },
        sharpe: if sd > 0.0 {
            m / sd * per_year.sqrt()
        } else {
            0.0
        },
        max_dd,
        by_year,
    }
}

/// Every `h` days from `start`: buy the `top` highest scores, short the
/// `top` lowest, enter next open, exit `h` days later at the open.
fn trade(p: &Panel, scores: &[Vec<Option<f64>>], start: usize, h: usize, args: &Args) -> Book {
    let n = p.symbols.len();
    let mut book = Book::default();
    let (mut prev_long, mut prev_short): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    let mut d = start;
    while d + 1 + h < p.dates.len() {
        let fwd: Vec<Option<f64>> = (0..n).map(|s| forward(p, s, d, h)).collect();
        let mut ranked: Vec<(usize, f64)> = (0..n)
            .filter_map(|s| Some((s, scores[d][s]?)))
            .filter(|(s, _)| fwd[*s].is_some())
            .collect();
        if ranked.len() >= MIN_STOCKS.max(2 * args.top) {
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
            let long: Vec<usize> = ranked.iter().take(args.top).map(|x| x.0).collect();
            let short: Vec<usize> = ranked.iter().rev().take(args.top).map(|x| x.0).collect();
            let avg =
                |set: &[usize]| set.iter().filter_map(|&s| fwd[s]).sum::<f64>() / set.len() as f64;
            let all: Vec<usize> = ranked.iter().map(|x| x.0).collect();
            let changed = |now: &[usize], before: &[usize]| {
                now.iter().filter(|s| !before.contains(s)).count() as f64 / now.len() as f64
            };
            let (tl, ts) = (changed(&long, &prev_long), changed(&short, &prev_short));
            let (l, s, u) = (avg(&long), avg(&short), avg(&all));
            let long_net = l - tl * args.delivery_bps / 10_000.0;
            let ls_net = l - s - (tl + ts) * args.futures_bps / 10_000.0;
            book.periods
                .push((p.dates[d + 1], long_net, ls_net, l - u, u));
            book.turnover.push(tl);
            prev_long = long;
            prev_short = short;
        }
        d += h;
    }
    book
}

/// Walk-forward weights: each signal's average rank IC over past days
/// whose `h`-day forward return had fully ended before `d`.
fn combined_scores(
    p: &Panel,
    ranked: &[Vec<Vec<Option<f64>>>],
    ics: &[Vec<Option<f64>>],
    h: usize,
    warmup: usize,
) -> (Vec<Vec<Option<f64>>>, Vec<Vec<f64>>) {
    let (nf, nd, n) = (ranked.len(), p.dates.len(), p.symbols.len());
    let mut scores = vec![vec![None; n]; nd];
    let mut weights_log = Vec::new();
    let mut sums = vec![0.0; nf];
    let mut counts = vec![0usize; nf];
    for d in 0..nd {
        // IC of day e is known once its forward window (to e + 1 + h) has ended.
        if let Some(e) = d.checked_sub(h + 1) {
            for f in 0..nf {
                if let Some(ic) = ics[f][e] {
                    sums[f] += ic;
                    counts[f] += 1;
                }
            }
        }
        if d < warmup {
            continue;
        }
        let w: Vec<f64> = (0..nf)
            .map(|f| {
                if counts[f] > 50 {
                    sums[f] / counts[f] as f64
                } else {
                    0.0
                }
            })
            .collect();
        for s in 0..n {
            let mut total = 0.0;
            let mut used = 0;
            for f in 0..nf {
                if let Some(r) = ranked[f][d][s] {
                    total += w[f] * r;
                    used += 1;
                }
            }
            if used * 2 >= nf {
                scores[d][s] = Some(total);
            }
        }
        weights_log.push(w);
    }
    (scores, weights_log)
}

fn pct(x: f64) -> String {
    format!("{:+.1}%", x * 100.0)
}

fn print_book(label: &str, book: &Book, h: usize) {
    let long: Vec<(NaiveDate, f64)> = book.periods.iter().map(|x| (x.0, x.1)).collect();
    let ls: Vec<(NaiveDate, f64)> = book.periods.iter().map(|x| (x.0, x.2)).collect();
    let uni: Vec<(NaiveDate, f64)> = book.periods.iter().map(|x| (x.0, x.4)).collect();
    let excess = mean_t(&book.periods.iter().map(|x| x.3).collect::<Vec<_>>());
    let turnover = mean_t(&book.turnover).0;
    let (a, b, c) = (summarize(&long, h), summarize(&ls, h), summarize(&uni, h));
    let years = |s: &Summary| {
        s.by_year
            .iter()
            .map(|(y, r)| format!("{}:{}", y % 100, pct(*r)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!(
        "{label:<14} h={h:<2} turnover {:>4.0}%  excess/trade {:>+6.1} bps (t {:>4.1})",
        turnover * 100.0,
        excess.0 * 10_000.0,
        excess.1
    );
    println!(
        "   long-only net: CAGR {:>7}  Sharpe {:>5.2}  maxDD {:>6}   {}",
        pct(a.cagr),
        a.sharpe,
        pct(-a.max_dd),
        years(&a)
    );
    println!(
        "   long-short net: CAGR {:>7}  Sharpe {:>5.2}  maxDD {:>6}   {}",
        pct(b.cagr),
        b.sharpe,
        pct(-b.max_dd),
        years(&b)
    );
    println!(
        "   universe (equal weight, no costs): CAGR {:>7}  Sharpe {:>5.2}  maxDD {:>6}",
        pct(c.cagr),
        c.sharpe,
        pct(-c.max_dd)
    );
}

fn run(args: &Args) -> Result<(), Error> {
    if args.refresh || fs::metadata(&args.cache).is_err() {
        build_cache(args)?;
    }
    let p = load_cache(&args.cache)?;
    let (n, nd) = (p.symbols.len(), p.dates.len());
    println!(
        "{} stocks, {} days ({} to {})",
        n,
        nd,
        p.dates.first().map_or(String::new(), ToString::to_string),
        p.dates.last().map_or(String::new(), ToString::to_string)
    );
    let breaks = p.breaks();
    println!(
        "history restarts (unadjusted corporate actions): {}",
        breaks.join(", ")
    );

    let fs_ = factors();
    // raw[f][d][s] and its cross-sectional ranks.
    let raw: Vec<Vec<Vec<Option<f64>>>> = fs_
        .iter()
        .map(|f| {
            (0..nd)
                .map(|d| (0..n).map(|s| (f.calc)(&p, s, d)).collect())
                .collect()
        })
        .collect();
    let ranked: Vec<Vec<Vec<Option<f64>>>> = raw
        .iter()
        .map(|days| days.iter().map(|x| ranks(x)).collect())
        .collect();

    for &h in &args.horizons {
        let fwd: Vec<Vec<Option<f64>>> = (0..nd)
            .map(|d| (0..n).map(|s| forward(&p, s, d, h)).collect())
            .collect();
        let ics: Vec<Vec<Option<f64>>> = raw
            .iter()
            .map(|days| (0..nd).map(|d| rank_ic(&days[d], &fwd[d])).collect())
            .collect();

        println!("\n=== hold {h} day(s): rank IC of each signal with the next {h}-day return");
        println!(
            "(IC: correlation between today's ranking and the coming return ranking, -1..1. 0.02+ with t above 3 is a usable signal.)"
        );
        println!(
            "{:<14} {:>7} {:>6}  IC by year   | about",
            "signal", "IC", "t"
        );
        for (f, factor) in fs_.iter().enumerate() {
            // Non-overlapping days for an honest t.
            let xs: Vec<f64> = ics[f].iter().step_by(h).filter_map(|x| *x).collect();
            let (m, t) = mean_t(&xs);
            let mut by_year: BTreeMap<i32, Vec<f64>> = BTreeMap::new();
            for (d, ic) in ics[f].iter().enumerate() {
                if let Some(ic) = ic {
                    by_year.entry(p.dates[d].year()).or_default().push(*ic);
                }
            }
            let years: Vec<String> = by_year
                .iter()
                .map(|(y, v)| format!("{}:{:+.3}", y % 100, mean_t(v).0))
                .collect();
            println!(
                "{:<14} {:>+7.3} {:>6.1}  {} | {}",
                factor.name,
                m,
                t,
                years.join(" "),
                factor.about
            );
        }

        let (combined, weights) = combined_scores(&p, &ranked, &ics, h, args.warmup);
        if let Some(w) = weights.last() {
            let mut ws: Vec<(f64, &str)> = w.iter().zip(&fs_).map(|(w, f)| (*w, f.name)).collect();
            ws.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
            let txt: Vec<String> = ws.iter().map(|(w, n)| format!("{n} {w:+.3}")).collect();
            println!(
                "\ncombined model weights at the end (learned walk-forward): {}",
                txt.join(", ")
            );
        }

        println!(
            "\nTrading it out of sample from day {} ({}), every {h} day(s), top/bottom {} stocks:",
            args.warmup,
            p.dates
                .get(args.warmup)
                .map_or(String::new(), ToString::to_string),
            args.top
        );
        print_book("COMBINED", &trade(&p, &combined, args.warmup, h, args), h);
        // The strongest single signals over the same period, for comparison:
        // a signal's direction is taken from its IC before the period starts.
        for (f, factor) in fs_.iter().enumerate() {
            let past: Vec<f64> = ics[f]
                .iter()
                .take(args.warmup.saturating_sub(h + 1))
                .filter_map(|x| *x)
                .collect();
            let (m, t) = mean_t(&past);
            if t.abs() < 2.0 {
                continue;
            }
            let sign = m.signum();
            let scores: Vec<Vec<Option<f64>>> = ranked[f]
                .iter()
                .map(|day| day.iter().map(|x| x.map(|v| v * sign)).collect())
                .collect();
            let label = format!("{}{}", if sign < 0.0 { "-" } else { "" }, factor.name);
            print_book(&label, &trade(&p, &scores, args.warmup, h, args), h);
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
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
    fn ranks_and_ic() {
        let r = ranks(&[Some(3.0), None, Some(1.0), Some(2.0)]);
        assert_eq!(r, vec![Some(0.5), None, Some(-0.5), Some(0.0)]);
        let a: Vec<Option<f64>> = (0..30).map(|i| Some(f64::from(i))).collect();
        let b: Vec<Option<f64>> = (0..30).map(|i| Some(f64::from(i * i))).collect();
        assert!((rank_ic(&a, &b).unwrap() - 1.0).abs() < 1e-9);
        let c: Vec<Option<f64>> = (0..30).map(|i| Some(-f64::from(i))).collect();
        assert!((rank_ic(&a, &c).unwrap() + 1.0).abs() < 1e-9);
    }

    fn day(px: f64) -> Option<Day> {
        Some(Day {
            open: px,
            high: px,
            low: px,
            close: px,
            volume: 1.0,
            vwap: px,
        })
    }

    #[test]
    fn breaks_and_gaps_split_history() {
        let d0 = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let mut rows = BTreeMap::new();
        // 100, 101, (missing), 102, 50 (split), 51
        for (i, px) in [
            Some(100.0),
            Some(101.0),
            None,
            Some(102.0),
            Some(50.0),
            Some(51.0),
        ]
        .into_iter()
        .enumerate()
        {
            rows.insert(
                d0 + chrono::TimeDelta::days(i as i64),
                vec![px.and_then(day)],
            );
        }
        let p = Panel::build(vec!["S".into()], rows);
        assert!(p.joined(0, 0, 1));
        assert!(!p.joined(0, 1, 3)); // missing day between
        assert!(!p.joined(0, 3, 4)); // split
        assert!(p.joined(0, 4, 5));
        assert_eq!(p.breaks().len(), 1);
        // Forward from day 3 (enter open of 4) for 1 day: 4 -> 5, after the split.
        assert!((forward(&p, 0, 3, 1).unwrap() - 0.02).abs() < 1e-9);
        assert!(forward(&p, 0, 2, 1).is_none()); // would span the split
    }

    #[test]
    fn summary_compounds() {
        let d = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let s = summarize(&[(d, 0.1), (d, -0.5), (d, 0.2)], 1);
        assert!((s.max_dd - 0.5).abs() < 1e-9);
        assert!((s.by_year[&2025] - (1.1 * 0.5 * 1.2 - 1.0)).abs() < 1e-9);
    }
}
