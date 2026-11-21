//! Event study: does the price move the way an idea expects after a
//! condition, by more than chance and more than costs?
//!
//! For every time the condition fires in history, the trade is entered at
//! the next candle's open (the decision needs the candle's close) and held
//! for each horizon, never past the 15:10 square-off. Returns are compared
//! with a baseline: the average move of the same stock, from the same minute
//! of the day, over every day. An idea is only interesting if it beats that
//! baseline by more than costs, in most years, on both sides.
//!
//! ```bash
//! cargo run --release -p tools --bin event-study -- --idea vwap-reversion
//! ```
//!
//! Uses the same history, session filter and indicator code as the
//! backtester, so the two cannot disagree about what the data said.

use std::collections::BTreeMap;
use std::process::ExitCode;

use backtest::{BarSource, QuestDbSource, load_universe_file};
use chrono::{Datelike, NaiveDate};
use clap::{Parser, ValueEnum};
use indicators::{BankBuilder, Handle, IndicatorBank, Registry};
use trading_core::{Bar, MarketHours, MinuteOfDay};

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Idea {
    /// Far from VWAP mid-session, expect a move back toward it.
    VwapReversion,
    /// A big opening gap from yesterday's close, expect it to fill.
    GapFade,
    /// At 10:14, stocks far behind (ahead of) the rest catch up (give back).
    RelativeStrength,
    /// A candle with several times its normal volume, expect the move to
    /// continue.
    VolumeShock,
}

impl Idea {
    /// What the thresholds measure.
    fn unit(self) -> &'static str {
        match self {
            Idea::VwapReversion => "distance from VWAP in daily ATRs",
            Idea::GapFade => "gap from yesterday's close, in %",
            Idea::RelativeStrength => "return since the open minus the median stock's, in %",
            Idea::VolumeShock => "candle volume as a multiple of normal for that minute",
        }
    }

    fn default_thresholds(self) -> Vec<f64> {
        match self {
            Idea::VwapReversion => vec![0.25, 0.5, 0.75, 1.0],
            Idea::GapFade => vec![0.5, 1.0, 2.0, 3.0],
            Idea::RelativeStrength => vec![0.5, 1.0, 1.5, 2.0],
            Idea::VolumeShock => vec![3.0, 5.0, 8.0],
        }
    }

    /// Holding times in minutes; the longest runs to the square-off.
    fn default_horizons(self) -> Vec<i64> {
        match self {
            Idea::VwapReversion | Idea::VolumeShock => vec![15, 30, 60],
            Idea::GapFade => vec![30, 60, 120, 355],
            Idea::RelativeStrength => vec![30, 60, 120, 295],
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Measure what the price does after an event, against a baseline and costs")]
struct Args {
    #[arg(long, value_enum)]
    idea: Idea,
    #[arg(long, default_value = "data/universe.csv")]
    universe: String,
    #[arg(long, default_value = "2022-01-01")]
    from: NaiveDate,
    #[arg(long, default_value = "2026-10-31")]
    to: NaiveDate,
    /// Round-trip cost in basis points: charges (about 10.8 bps on a
    /// ₹30,000 trade, measured) plus slippage on both sides.
    #[arg(long, default_value_t = 15.0)]
    cost_bps: f64,
    /// Holding times to measure, in minutes. Default depends on the idea.
    #[arg(long, value_delimiter = ',')]
    horizons: Option<Vec<i64>>,
    /// Event strengths to test; the unit depends on the idea.
    #[arg(long, value_delimiter = ',')]
    thresholds: Option<Vec<f64>>,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

/// Mid-session event decisions only in this window (IST), away from the
/// open's noise and leaving time before the 15:10 square-off.
const FIRST_DECISION: (u16, u16) = (10, 0);
const LAST_DECISION: (u16, u16) = (14, 0);
/// Volume shocks from here: the first 15 minutes are always heavy.
const FIRST_VOLUME_DECISION: (u16, u16) = (9, 30);
const SQUARE_OFF: (u16, u16) = (15, 10);
/// Relative strength is measured at the close of this candle.
const RS_DECISION: (u16, u16) = (10, 14);
/// A volume shock also needs the candle to move at least this much (%).
const SHOCK_MIN_MOVE_PCT: f64 = 0.1;
/// Gaps beyond this are taken as unadjusted corporate actions (splits,
/// bonuses, demergers), not trading.
const MAX_REAL_GAP_PCT: f64 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Long,
    Short,
}

impl Side {
    fn sign(self) -> f64 {
        match self {
            Side::Long => 1.0,
            Side::Short => -1.0,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Side::Long => "long",
            Side::Short => "short",
        }
    }
    fn index(self) -> usize {
        usize::from(self == Side::Short)
    }
}

/// One firing of the condition, with the raw (unsigned) forward returns.
struct Event {
    variant: usize,
    side: Side,
    stock: usize,
    entry_minute: u16,
    year: i32,
    /// Per horizon: raw return in bps from entry, `None` if no exit bar.
    returns: Vec<Option<f64>>,
}

/// Sum and count of raw forward returns per (stock, entry minute, horizon).
struct Baseline {
    horizons: usize,
    sums: Vec<f64>,
    counts: Vec<u32>,
}

impl Baseline {
    const MINUTES: usize = 400;

    fn new(stocks: usize, horizons: usize) -> Self {
        let n = stocks * Self::MINUTES * horizons;
        Self {
            horizons,
            sums: vec![0.0; n],
            counts: vec![0; n],
        }
    }

    fn slot(&self, stock: usize, minute: u16, h: usize) -> usize {
        (stock * Self::MINUTES + usize::from(minute).min(Self::MINUTES - 1)) * self.horizons + h
    }

    fn add(&mut self, stock: usize, minute: u16, h: usize, r: f64) {
        let i = self.slot(stock, minute, h);
        if let (Some(s), Some(c)) = (self.sums.get_mut(i), self.counts.get_mut(i)) {
            *s += r;
            *c += 1;
        }
    }

    fn mean(&self, stock: usize, minute: u16, h: usize) -> Option<f64> {
        let i = self.slot(stock, minute, h);
        let (s, c) = (self.sums.get(i)?, self.counts.get(i)?);
        (*c > 0).then(|| s / f64::from(*c))
    }
}

/// Raw return in bps from the open of `bars[entry]` to the close of the
/// last bar before `entry + horizon` minutes and before the square-off.
fn forward(bars: &[Bar], entry: usize, horizon: i64) -> Option<f64> {
    let start = bars.get(entry)?;
    let end_ts = start.ts.plus_minutes(horizon);
    let square_off = MinuteOfDay::new(SQUARE_OFF.0, SQUARE_OFF.1)?;
    let exit = bars
        .get(entry..)?
        .iter()
        .take_while(|b| b.ts < end_ts && b.ts.ist_minute_of_day() < square_off)
        .last()?;
    let (p0, p1) = (start.open.as_f64(), exit.close.as_f64());
    (p0 > 0.0).then(|| (p1 / p0 - 1.0) * 10_000.0)
}

/// Median of a non-empty sample.
fn median(xs: &mut [f64]) -> Option<f64> {
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    if n == 0 {
        return None;
    }
    let hi = *xs.get(n / 2)?;
    if n % 2 == 1 {
        Some(hi)
    } else {
        Some((xs.get(n / 2 - 1)? + hi) / 2.0)
    }
}

/// Crossing test on a running value: which side fires when `x` moves
/// through `k` (above: `above` side, below `-k`: the other).
fn crossed(prev: Option<f64>, x: f64, k: f64, above: Side) -> Option<Side> {
    let below = if above == Side::Long {
        Side::Short
    } else {
        Side::Long
    };
    match prev {
        Some(p) if x >= k && p < k => Some(above),
        Some(p) if x <= -k && p > -k => Some(below),
        _ => None,
    }
}

struct Handles {
    vwap: Handle,
    atr: Handle,
    prev_close: Handle,
    vol_tod: Handle,
}

struct Study {
    idea: Idea,
    thresholds: Vec<f64>,
    horizons: Vec<i64>,
    hours: MarketHours,
    bank: IndicatorBank,
    h: Handles,
    baseline: Baseline,
    events: Vec<Event>,
}

impl Study {
    /// Records an event entered at `bars[entry]`'s open.
    fn push(&mut self, stock: usize, bars: &[Bar], entry: usize, variant: usize, side: Side) {
        let Some(next) = bars.get(entry) else {
            return;
        };
        let Some(entry_minute) = self.hours.session_minute(next.ts) else {
            return;
        };
        let returns = self
            .horizons
            .iter()
            .map(|&h| forward(bars, entry, h))
            .collect();
        self.events.push(Event {
            variant,
            side,
            stock,
            entry_minute,
            year: next.ts.ist_date().map_or(0, |d| d.year()),
            returns,
        });
    }

    fn day(&mut self, stocks: &[Vec<Bar>]) {
        let window = |from: (u16, u16)| {
            (
                MinuteOfDay::new(from.0, from.1),
                MinuteOfDay::new(LAST_DECISION.0, LAST_DECISION.1),
            )
        };
        let (first, last) = window(if self.idea == Idea::VolumeShock {
            FIRST_VOLUME_DECISION
        } else {
            FIRST_DECISION
        });
        let rs_at = MinuteOfDay::new(RS_DECISION.0, RS_DECISION.1);
        // Relative strength: (stock, entry index, return since open in %).
        let mut rs: Vec<(usize, usize, f64)> = Vec::new();
        let variants = self.thresholds.len();

        for (stock, bars) in stocks.iter().enumerate() {
            let mut prev_z: Option<f64> = None;
            // Which sides already fired today, per variant.
            let mut fired = vec![[false; 2]; variants];
            for (i, bar) in bars.iter().enumerate() {
                if self.bank.update(bar).is_err() {
                    continue;
                }
                let entry = i + 1;
                let Some(next) = bars.get(entry) else {
                    continue;
                };
                let Some(entry_minute) = self.hours.session_minute(next.ts) else {
                    continue;
                };
                // Baseline: every possible entry, every horizon.
                for (h, &horizon) in self.horizons.iter().enumerate() {
                    if let Some(r) = forward(bars, entry, horizon) {
                        self.baseline.add(stock, entry_minute, h, r);
                    }
                }

                let m = bar.ts.ist_minute_of_day();
                let mut fire: Vec<(usize, Side)> = Vec::new();
                match self.idea {
                    Idea::GapFade => {
                        // Decided on the day's first candle: its open is
                        // the gap. Entered at the second candle's open.
                        if i != 0 || self.hours.session_minute(bar.ts) != Some(0) {
                            continue;
                        }
                        let Some(pc) = self.bank.get(bar.instrument, self.h.prev_close) else {
                            continue;
                        };
                        if pc <= 0.0 {
                            continue;
                        }
                        let gap = (bar.open.as_f64() / pc - 1.0) * 100.0;
                        if gap.abs() > MAX_REAL_GAP_PCT {
                            continue;
                        }
                        for (v, &k) in self.thresholds.iter().enumerate() {
                            if gap >= k {
                                fire.push((v, Side::Short));
                            } else if gap <= -k {
                                fire.push((v, Side::Long));
                            }
                        }
                    }
                    Idea::RelativeStrength => {
                        if rs_at == Some(m)
                            && let Some(first_bar) = bars.first()
                        {
                            let o = first_bar.open.as_f64();
                            if o > 0.0 && self.hours.session_minute(first_bar.ts) == Some(0) {
                                rs.push((stock, entry, (bar.close.as_f64() / o - 1.0) * 100.0));
                            }
                        }
                        continue;
                    }
                    Idea::VwapReversion | Idea::VolumeShock => {
                        if first.is_none_or(|f| m < f) || last.is_none_or(|l| m > l) {
                            prev_z = None;
                            continue;
                        }
                        if self.idea == Idea::VwapReversion {
                            let (Some(vwap), Some(atr)) = (
                                self.bank.get(bar.instrument, self.h.vwap),
                                self.bank.get(bar.instrument, self.h.atr),
                            ) else {
                                continue;
                            };
                            if atr <= 0.0 {
                                continue;
                            }
                            let z = (bar.close.as_f64() - vwap) / atr;
                            for (v, &k) in self.thresholds.iter().enumerate() {
                                if let Some(side) = crossed(prev_z, z, k, Side::Short) {
                                    fire.push((v, side));
                                }
                            }
                            prev_z = Some(z);
                        } else {
                            let Some(normal) = self.bank.get(bar.instrument, self.h.vol_tod) else {
                                continue;
                            };
                            let o = bar.open.as_f64();
                            if normal <= 0.0 || o <= 0.0 {
                                continue;
                            }
                            let ratio = bar.volume as f64 / normal;
                            let mv = (bar.close.as_f64() / o - 1.0) * 100.0;
                            if mv.abs() < SHOCK_MIN_MOVE_PCT {
                                continue;
                            }
                            let side = if mv > 0.0 { Side::Long } else { Side::Short };
                            for (v, &k) in self.thresholds.iter().enumerate() {
                                if ratio >= k {
                                    fire.push((v, side));
                                }
                            }
                        }
                    }
                }
                for (v, side) in fire {
                    let Some(done) = fired.get_mut(v).and_then(|f| f.get_mut(side.index())) else {
                        continue;
                    };
                    if !*done {
                        *done = true;
                        self.push(stock, bars, entry, v, side);
                    }
                }
            }
        }

        // Relative strength needs every stock's 10:14 return first. Needs
        // most of the universe present, or the median means little.
        if rs.len() * 2 < stocks.len() {
            return;
        }
        let mut rets: Vec<f64> = rs.iter().map(|r| r.2).collect();
        let Some(mid) = median(&mut rets) else {
            return;
        };
        for (stock, entry, r) in rs {
            let diff = r - mid;
            for v in 0..variants {
                let Some(&k) = self.thresholds.get(v) else {
                    continue;
                };
                // Laggards bought, leaders sold: the catch-up idea.
                let side = if diff <= -k {
                    Side::Long
                } else if diff >= k {
                    Side::Short
                } else {
                    continue;
                };
                if let Some(bars) = stocks.get(stock) {
                    self.push(stock, bars, entry, v, side);
                }
            }
        }
    }
}

/// Mean, t-statistic and count of a sample.
fn stats(xs: &[f64]) -> (f64, f64, usize) {
    let n = xs.len();
    if n < 2 {
        return (xs.first().copied().unwrap_or(0.0), 0.0, n);
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let t = mean / (var.sqrt() / (n as f64).sqrt());
    (mean, t, n)
}

fn report(study: &Study, cost: f64) {
    println!(
        "\nAll figures are averages per trade in basis points (1 bp = 0.01%). \
         'vs base' is the move beyond what that stock usually does from that minute; \
         'net' takes {cost} bps of costs off 'vs base'; t above 2 or below -2 is unlikely to be chance.\n\
         thresh = {}\n",
        study.idea.unit()
    );
    println!(
        "{:<7} {:<6} {:>4} {:>7} {:>8} {:>8} {:>8} {:>6} {:>8} {:>8}  net by year",
        "thresh", "side", "hold", "events", "gross", "vs base", "net", "t", "win%", "net@2x"
    );
    for (v, &k) in study.thresholds.iter().enumerate() {
        for side in [Side::Long, Side::Short] {
            for (h, &horizon) in study.horizons.iter().enumerate() {
                let mut gross = Vec::new();
                let mut excess = Vec::new();
                let mut by_year: BTreeMap<i32, Vec<f64>> = BTreeMap::new();
                for e in study
                    .events
                    .iter()
                    .filter(|e| e.variant == v && e.side == side)
                {
                    let (Some(Some(r)), Some(base)) = (
                        e.returns.get(h),
                        study.baseline.mean(e.stock, e.entry_minute, h),
                    ) else {
                        continue;
                    };
                    let g = side.sign() * r;
                    let x = g - side.sign() * base;
                    gross.push(g);
                    excess.push(x);
                    by_year.entry(e.year).or_default().push(x - cost);
                }
                let (g, _, n) = stats(&gross);
                let (x, t, _) = stats(&excess);
                let win = if n == 0 {
                    0.0
                } else {
                    gross.iter().filter(|&&r| r > cost).count() as f64 / n as f64 * 100.0
                };
                let years: Vec<String> = by_year
                    .iter()
                    .map(|(y, xs)| format!("{}:{:+.0}", y % 100, stats(xs).0))
                    .collect();
                println!(
                    "{:<7} {:<6} {:>4} {:>7} {:>8.1} {:>8.1} {:>8.1} {:>6.1} {:>7.1}% {:>8.1}  {}",
                    k,
                    side.name(),
                    horizon,
                    n,
                    g,
                    x,
                    x - cost,
                    t,
                    win,
                    x - 2.0 * cost,
                    years.join(" ")
                );
            }
        }
    }
}

fn run(args: &Args) -> Result<(), Error> {
    let (universe, ids) = load_universe_file(&args.universe)?;
    let mut builder = BankBuilder::new(Registry::builtin());
    let h = Handles {
        vwap: builder.require("vwap")?,
        atr: builder.require("atr_daily(14)")?,
        prev_close: builder.require("prev_day.close")?,
        vol_tod: builder.require("vol_tod(20)")?,
    };
    let horizons = args
        .horizons
        .clone()
        .unwrap_or_else(|| args.idea.default_horizons());
    let mut study = Study {
        idea: args.idea,
        thresholds: args
            .thresholds
            .clone()
            .unwrap_or_else(|| args.idea.default_thresholds()),
        baseline: Baseline::new(universe.len(), horizons.len()),
        horizons,
        hours: MarketHours::NSE_EQUITY,
        bank: builder.build(universe.len())?,
        h,
        events: Vec::new(),
    };
    let mut source = QuestDbSource::connect(&args.qdb, ids, args.from, args.to)?;
    let mut days = 0;
    while let Some((_, bars)) = source.next_session()? {
        let mut stocks: Vec<Vec<Bar>> = vec![Vec::new(); universe.len()];
        for bar in bars {
            if let Some(s) = stocks.get_mut(bar.instrument.index()) {
                s.push(bar);
            }
        }
        study.day(&stocks);
        days += 1;
    }
    println!(
        "{:?}: {days} days, {} stocks, {} events, cost {} bps per round trip",
        args.idea,
        universe.len(),
        study.events.len(),
        args.cost_bps
    );
    report(&study, args.cost_bps);
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
    use trading_core::{InstrumentId, Price};

    fn bars(closes: &[f64]) -> Vec<Bar> {
        let open = MarketHours::NSE_EQUITY
            .open_at(NaiveDate::from_ymd_opt(2025, 1, 6).unwrap())
            .unwrap();
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                let p = Price::from_rupees_f64(c).unwrap();
                Bar {
                    instrument: InstrumentId(0),
                    ts: open.plus_minutes(i as i64),
                    open: p,
                    high: p,
                    low: p,
                    close: p,
                    volume: 1,
                }
            })
            .collect()
    }

    #[test]
    fn forward_return_holds_for_the_horizon() {
        let b = bars(&[100.0, 100.0, 101.0, 102.0, 103.0]);
        // Enter at bar 1's open (100), hold 2 minutes: exit at bar 2's close.
        let r = forward(&b, 1, 2).unwrap();
        assert!((r - 100.0).abs() < 1e-9); // +1% = 100 bps
    }

    #[test]
    fn forward_return_stops_at_square_off() {
        // 360 bars: minute 355 is 15:10.
        let closes: Vec<f64> = (0..360).map(|i| 100.0 + f64::from(i) * 0.01).collect();
        let b = bars(&closes);
        let r = forward(&b, 350, 60).unwrap();
        // Exit at the 15:09 bar (index 354): 103.54 / 103.50.
        let expected = (103.54 / 103.50 - 1.0) * 10_000.0;
        assert!((r - expected).abs() < 1e-6);
    }

    #[test]
    fn median_and_crossing() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&mut []), None);
        assert_eq!(crossed(Some(0.4), 0.6, 0.5, Side::Short), Some(Side::Short));
        assert_eq!(
            crossed(Some(-0.4), -0.6, 0.5, Side::Short),
            Some(Side::Long)
        );
        assert_eq!(crossed(Some(0.6), 0.7, 0.5, Side::Short), None);
        assert_eq!(crossed(None, 0.7, 0.5, Side::Short), None);
    }

    #[test]
    fn stats_are_sane() {
        let (m, t, n) = stats(&[1.0, 2.0, 3.0]);
        assert_eq!((m, n), (2.0, 3));
        assert!(t > 3.0);
    }
}
