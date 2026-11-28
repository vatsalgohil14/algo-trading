//! The rotation itself: universe on a day, holdings on a rebalance, the
//! backtest that repeats it, and the plan for one real rebalance.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::RotationError;
use crate::panel::{MONTH, Panel, TRADING_DAYS};
use crate::strategy::Ranker;

/// Which stocks may be held on a day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UniverseConfig {
    /// The `size` stocks with the highest median daily traded value...
    pub size: usize,
    /// ...over this many days before the rebalance.
    pub liquidity_days: usize,
    /// Rupees.
    pub min_price: f64,
    /// Days of unbroken history required.
    pub history_days: usize,
}

impl Default for UniverseConfig {
    fn default() -> Self {
        Self {
            size: 500,
            liquidity_days: 60,
            min_price: 20.0,
            history_days: 12 * MONTH,
        }
    }
}

/// When to rebalance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    /// On the close of the first trading day of each calendar month (and on
    /// the first day of a run). The same days live and in a backtest, so a
    /// month's plan can be checked against the backtest.
    MonthStart,
    /// Every this many trading days from the first day (research).
    EveryDays(usize),
}

impl Schedule {
    /// Whether day `d` is a rebalance day, given the last one (`None` =
    /// none yet, so rebalance now).
    pub fn due(self, p: &Panel, d: usize, last: Option<usize>) -> bool {
        let Some(last) = last else {
            return true;
        };
        if d <= last {
            return false;
        }
        match self {
            Self::MonthStart => match (p.date(d), p.date(last)) {
                (Some(a), Some(b)) => (a.year(), a.month()) != (b.year(), b.month()),
                _ => false,
            },
            Self::EveryDays(n) => d - last >= n,
        }
    }

    /// Rebalances per year, roughly (for reports).
    pub fn per_year(self) -> f64 {
        match self {
            Self::MonthStart => 12.0,
            Self::EveryDays(n) => TRADING_DAYS / n.max(1) as f64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RotationConfig {
    pub universe: UniverseConfig,
    /// Stocks held, in equal amounts.
    pub hold: usize,
    pub schedule: Schedule,
    /// Round trip per rupee replaced, bps: delivery STT 0.1% each side,
    /// stamp, exchange and DP charges, mid-cap slippage.
    pub cost_bps: f64,
    /// With a capital amount, a stock is skipped (the next one is taken)
    /// when its position would exceed this fraction of its median daily
    /// traded value, so it can be bought and sold within a day.
    pub max_daily_value_fraction: f64,
    /// Rank buffer: a stock already held is kept while it ranks within the
    /// top `exit_rank`, not only the top `hold`. Cuts selling on small rank
    /// changes. `None` = sell as soon as it leaves the top `hold`.
    pub exit_rank: Option<usize>,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            universe: UniverseConfig::default(),
            hold: 30,
            schedule: Schedule::MonthStart,
            cost_bps: 40.0,
            max_daily_value_fraction: 0.02,
            exit_rank: None,
        }
    }
}

impl RotationConfig {
    /// The settings the strategy is run with: top 30, rebalanced on the
    /// first trading day of each month, held stocks kept while they rank in
    /// the top 60 (see `docs/strategies/rotation.md`).
    pub fn recommended() -> Self {
        Self {
            exit_rank: Some(60),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), RotationError> {
        let u = &self.universe;
        let ok = self.hold > 0
            && self.hold <= u.size
            && self.schedule != Schedule::EveryDays(0)
            && u.liquidity_days >= 5
            && self.cost_bps.is_finite()
            && self.cost_bps >= 0.0
            && u.min_price.is_finite()
            && self.max_daily_value_fraction > 0.0
            && self.max_daily_value_fraction <= 1.0
            && self.exit_rank.is_none_or(|r| r >= self.hold);
        if ok {
            Ok(())
        } else {
            Err(RotationError::Config(format!(
                "invalid rotation settings: {self:?}"
            )))
        }
    }
}

/// Point-in-time universe on day `d`, most liquid first.
pub fn universe(p: &Panel, d: usize, c: &UniverseConfig) -> Vec<usize> {
    let Some(back) = d.checked_sub(c.history_days) else {
        return Vec::new();
    };
    let mut liq: Vec<(usize, f64)> = (0..p.stocks())
        .filter_map(|s| {
            let today = p.get(s, d)?;
            (today.close >= c.min_price && p.joined(s, back, d)).then_some(())?;
            Some((s, p.median_value(s, d, c.liquidity_days)?))
        })
        .collect();
    liq.sort_by(|a, b| b.1.total_cmp(&a.1));
    liq.into_iter().take(c.size).map(|x| x.0).collect()
}

/// The universe on day `d` with scores, best first. Stocks the ranker
/// cannot score (too little history) are left out.
pub fn ranking(p: &Panel, ranker: &dyn Ranker, c: &RotationConfig, d: usize) -> Vec<(usize, f64)> {
    let mut ranked: Vec<(usize, f64)> = universe(p, d, &c.universe)
        .into_iter()
        .filter_map(|s| Some((s, ranker.score(p, s, d)?)))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked
}

/// The stocks to hold from day `d`: the highest scores in the universe,
/// skipping any too illiquid for `capital` (if given). Stocks in `held` are
/// kept while they rank within `exit_rank` (the rank buffer); the remaining
/// places go to the best of the rest. Kept stocks come first.
pub fn select(
    p: &Panel,
    ranker: &dyn Ranker,
    c: &RotationConfig,
    d: usize,
    capital: Option<f64>,
    held: &[usize],
) -> Vec<usize> {
    let ranked = ranking(p, ranker, c, d);
    let per_stock = capital.map(|cap| cap / c.hold as f64);
    let eligible: Vec<usize> = ranked
        .into_iter()
        .filter(|&(s, _)| {
            per_stock.is_none_or(|amount| {
                p.median_value(s, d, c.universe.liquidity_days)
                    .is_some_and(|v| amount <= v * c.max_daily_value_fraction)
            })
        })
        .map(|x| x.0)
        .collect();
    let keep_within = c.exit_rank.unwrap_or(c.hold);
    let mut picks: Vec<usize> = eligible
        .iter()
        .take(keep_within)
        .filter(|s| held.contains(s))
        .take(c.hold)
        .copied()
        .collect();
    for &s in &eligible {
        if picks.len() >= c.hold {
            break;
        }
        if !picks.contains(&s) {
            picks.push(s);
        }
    }
    picks
}

/// One holding period of a backtest.
#[derive(Debug, Clone, PartialEq)]
pub struct Period {
    /// Entry: the open after the rebalance day.
    pub entry: NaiveDate,
    /// Exit: the open after the next rebalance day.
    pub exit: NaiveDate,
    /// Equal-weight return of the holdings, before costs.
    pub gross: f64,
    /// Fraction of the portfolio replaced at entry.
    pub turnover: f64,
    pub holdings: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Backtest {
    pub periods: Vec<Period>,
    pub cost_bps: f64,
}

impl Backtest {
    /// Net return of each period after costs.
    pub fn net(&self) -> Vec<(NaiveDate, f64)> {
        self.periods
            .iter()
            .map(|x| (x.entry, x.gross - x.turnover * self.cost_bps / 10_000.0))
            .collect()
    }

    pub fn summary(&self) -> Summary {
        Summary::of(&self.net(), &self.periods)
    }
}

/// Runs the rotation from the first rebalance on or after `from` to `to`.
/// Signals use data up to each rebalance day's close; trades happen at the
/// next open. `ranker = None` holds the whole universe (the benchmark).
pub fn backtest(
    p: &Panel,
    ranker: Option<&dyn Ranker>,
    c: &RotationConfig,
    from: NaiveDate,
    to: NaiveDate,
    capital: Option<f64>,
) -> Result<Backtest, RotationError> {
    c.validate()?;
    let start = p
        .day_on_or_after(from)
        .ok_or_else(|| RotationError::Data(format!("no data on or after {from}")))?;
    let end = p
        .day_on_or_before(to)
        .ok_or_else(|| RotationError::Data(format!("no data before {to}")))?;
    let mut days: Vec<usize> = Vec::new();
    for d in start..=end {
        if c.schedule.due(p, d, days.last().copied()) {
            days.push(d);
        }
    }
    let mut periods = Vec::new();
    // Held into the next rebalance: this period's picks that had prices.
    let mut prev: Vec<usize> = Vec::new();
    for pair in days.windows(2) {
        let &[d, next] = pair else { continue };
        let (entry, exit) = (d + 1, next + 1);
        let (Some(entry_date), Some(exit_date)) = (p.date(entry), p.date(exit)) else {
            break;
        };
        let picks = match ranker {
            Some(r) => select(p, r, c, d, capital, &prev),
            None => universe(p, d, &c.universe),
        };
        let held: Vec<(usize, f64)> = picks
            .iter()
            .filter_map(|&s| Some((s, p.hold_return(s, entry, exit)?)))
            .collect();
        let gross = if held.is_empty() {
            0.0
        } else {
            held.iter().map(|x| x.1).sum::<f64>() / held.len() as f64
        };
        let turnover = match (prev.is_empty(), picks.is_empty()) {
            (_, true) => 0.0,
            (true, false) => 1.0,
            (false, false) => {
                picks.iter().filter(|s| !prev.contains(s)).count() as f64 / picks.len() as f64
            }
        };
        periods.push(Period {
            entry: entry_date,
            exit: exit_date,
            gross,
            turnover,
            holdings: picks.clone(),
        });
        prev = held.iter().map(|x| x.0).collect();
    }
    Ok(Backtest {
        periods,
        cost_bps: if ranker.is_some() { c.cost_bps } else { 0.0 },
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub cagr: f64,
    pub volatility: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub worst_year: f64,
    /// Portfolio turns per year (1.0 = everything replaced once).
    pub turnover_per_year: f64,
    pub by_year: BTreeMap<i32, f64>,
    pub periods: usize,
}

impl Summary {
    fn of(net: &[(NaiveDate, f64)], periods: &[Period]) -> Self {
        // Annualise by the calendar time covered, which works for any schedule.
        let span_days = match (periods.first(), periods.last()) {
            (Some(a), Some(b)) => (b.exit - a.entry).num_days() as f64,
            _ => 0.0,
        };
        let years = span_days / 365.25;
        let per_year = if years > 0.0 {
            net.len() as f64 / years
        } else {
            1.0
        };
        let (mut eq, mut peak, mut dd) = (1.0_f64, 1.0_f64, 0.0_f64);
        let mut by_year: BTreeMap<i32, f64> = BTreeMap::new();
        for &(date, x) in net {
            eq *= 1.0 + x;
            peak = peak.max(eq);
            dd = dd.max(1.0 - eq / peak);
            *by_year.entry(date.year()).or_insert(1.0) *= 1.0 + x;
        }
        for v in by_year.values_mut() {
            *v -= 1.0;
        }
        let (m, sd) = mean_sd(&net.iter().map(|x| x.1).collect::<Vec<_>>());
        let n = net.len() as f64;
        Self {
            cagr: if years > 0.0 && eq > 0.0 {
                eq.powf(1.0 / years) - 1.0
            } else {
                0.0
            },
            volatility: sd * per_year.sqrt(),
            sharpe: if sd > 0.0 {
                m / sd * per_year.sqrt()
            } else {
                0.0
            },
            max_drawdown: dd,
            worst_year: by_year.values().copied().reduce(f64::min).unwrap_or(0.0),
            turnover_per_year: periods.iter().map(|x| x.turnover).sum::<f64>() / n.max(1.0)
                * per_year,
            by_year,
            periods: net.len(),
        }
    }
}

fn mean_sd(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    if xs.len() < 2 {
        return (xs.first().copied().unwrap_or(0.0), 0.0);
    }
    let m = xs.iter().sum::<f64>() / n;
    (
        m,
        (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)).sqrt(),
    )
}

/// Average net gain per period over `bench` (same rebalance days), and its
/// t-statistic.
pub fn excess(a: &Backtest, bench: &Backtest) -> (f64, f64) {
    let b: BTreeMap<NaiveDate, f64> = bench.net().into_iter().collect();
    let xs: Vec<f64> = a
        .net()
        .into_iter()
        .filter_map(|(d, x)| Some(x - b.get(&d)?))
        .collect();
    let (m, sd) = mean_sd(&xs);
    (
        m,
        if sd > 0.0 {
            m / sd * (xs.len() as f64).sqrt()
        } else {
            0.0
        },
    )
}

/// One stock to hold after a rebalance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    pub rank: usize,
    pub symbol: String,
    /// Last close, rupees.
    pub price: f64,
    pub shares: u64,
    pub rupees: f64,
    pub score: f64,
    /// The position as a fraction of the stock's median daily traded value.
    pub share_of_daily_value: f64,
}

/// What to hold after rebalancing on day `d` with `capital` rupees: equal
/// amounts, whole shares at the last close.
pub fn plan(
    p: &Panel,
    ranker: &dyn Ranker,
    c: &RotationConfig,
    d: usize,
    capital: f64,
    held: &[String],
) -> Result<Vec<Target>, RotationError> {
    c.validate()?;
    if !(capital.is_finite() && capital > 0.0) {
        return Err(RotationError::Config("capital must be positive".into()));
    }
    let per = capital / c.hold as f64;
    let held: Vec<usize> = (0..p.stocks())
        .filter(|&s| p.symbol(s).is_some_and(|x| held.iter().any(|h| h == x)))
        .collect();
    select(p, ranker, c, d, Some(capital), &held)
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let price = p
                .get(s, d)
                .map(|x| x.close)
                .ok_or_else(|| RotationError::Data("no price".into()))?;
            let shares = (per / price).floor().max(0.0) as u64;
            let value = p
                .median_value(s, d, c.universe.liquidity_days)
                .unwrap_or(f64::INFINITY);
            Ok(Target {
                rank: i + 1,
                symbol: p.symbol(s).unwrap_or("?").to_owned(),
                price,
                shares,
                rupees: shares as f64 * price,
                score: ranker.score(p, s, d).unwrap_or(f64::NAN),
                share_of_daily_value: shares as f64 * price / value,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trade {
    pub symbol: String,
    pub side: Side,
    pub shares: u64,
}

/// Orders that turn `current` holdings (symbol, shares) into `targets`.
/// Sells come first, so their proceeds fund the buys.
pub fn trades(targets: &[Target], current: &[(String, u64)]) -> Vec<Trade> {
    let want: BTreeMap<&str, u64> = targets
        .iter()
        .map(|t| (t.symbol.as_str(), t.shares))
        .collect();
    let have: BTreeMap<&str, u64> = current.iter().map(|(s, q)| (s.as_str(), *q)).collect();
    let mut sells = Vec::new();
    let mut buys = Vec::new();
    for (sym, &h) in &have {
        let w = want.get(sym).copied().unwrap_or(0);
        if h > w {
            sells.push(Trade {
                symbol: (*sym).to_owned(),
                side: Side::Sell,
                shares: h - w,
            });
        }
    }
    for (sym, &w) in &want {
        let h = have.get(sym).copied().unwrap_or(0);
        if w > h {
            buys.push(Trade {
                symbol: (*sym).to_owned(),
                side: Side::Buy,
                shares: w - h,
            });
        }
    }
    sells.extend(buys);
    sells
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::panel::tests::panel;
    use crate::strategy::{Momentum, MomentumConfig};

    /// Stock i grows at i * 0.05% a day: higher i = stronger momentum.
    fn trending(stocks: usize, days: usize) -> Panel {
        let rows: Vec<Vec<Option<f64>>> = (0..stocks)
            .map(|i| {
                (0..days)
                    .map(|d| Some(100.0 * (1.0 + i as f64 * 0.0005).powi(d as i32)))
                    .collect()
            })
            .collect();
        let refs: Vec<&[Option<f64>]> = rows.iter().map(Vec::as_slice).collect();
        panel(&refs)
    }

    fn config(hold: usize) -> RotationConfig {
        RotationConfig {
            universe: UniverseConfig {
                size: 10,
                min_price: 1.0,
                ..UniverseConfig::default()
            },
            hold,
            ..RotationConfig::default()
        }
    }

    #[test]
    fn momentum_holds_the_strongest_and_beats_the_benchmark() {
        let p = trending(10, 400);
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        let c = config(3);
        let picks = select(&p, &m, &c, 300, None, &[]);
        assert_eq!(picks, vec![9, 8, 7]);
        let (from, to) = (p.date(260).unwrap(), p.date(399).unwrap());
        let bt = backtest(&p, Some(&m), &c, from, to, None).unwrap();
        let bench = backtest(&p, None, &c, from, to, None).unwrap();
        assert!(bt.periods.len() >= 5);
        assert_eq!(bt.periods[1].turnover, 0.0); // same picks every month
        let (ex, _) = excess(&bt, &bench);
        assert!(ex > 0.0);
        assert!(bt.summary().cagr > bench.summary().cagr);
    }

    #[test]
    fn rank_buffer_keeps_held_stocks_that_slip_a_little() {
        let p = trending(10, 400);
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        // S6 ranks 4th: sold without a buffer, kept with exit_rank 4.
        let plain = config(3);
        assert_eq!(select(&p, &m, &plain, 300, None, &[6]), vec![9, 8, 7]);
        let buffered = RotationConfig {
            exit_rank: Some(4),
            ..config(3)
        };
        assert_eq!(select(&p, &m, &buffered, 300, None, &[6]), vec![6, 9, 8]);
        // Outside the buffer it goes.
        assert_eq!(select(&p, &m, &buffered, 300, None, &[2]), vec![9, 8, 7]);
        assert!(
            RotationConfig {
                exit_rank: Some(2),
                ..config(3)
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn month_start_schedule_rebalances_on_each_new_month() {
        // The test panel has one row per calendar day from 2020-01-01.
        let p = trending(10, 400);
        let s = Schedule::MonthStart;
        assert!(s.due(&p, 5, None)); // first day of a run
        assert!(!s.due(&p, 20, Some(5)));
        assert!(s.due(&p, 31, Some(5))); // 2020-02-01
        assert!(!s.due(&p, 31, Some(31)));
        assert!(Schedule::EveryDays(21).due(&p, 26, Some(5)));
        assert!(!Schedule::EveryDays(21).due(&p, 25, Some(5)));
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        let (from, to) = (p.date(260).unwrap(), p.date(399).unwrap());
        let bt = backtest(&p, Some(&m), &config(3), from, to, None).unwrap();
        for w in bt.periods.windows(2) {
            assert_eq!(w[0].exit, w[1].entry);
        }
        // Entries are the day after the 1st of each month.
        assert!(bt.periods.iter().skip(1).all(|x| x.entry.day() == 2));
        let r = RotationConfig::recommended();
        assert_eq!(
            (r.hold, r.exit_rank, r.schedule),
            (30, Some(60), Schedule::MonthStart)
        );
        assert!(
            RotationConfig {
                schedule: Schedule::EveryDays(0),
                ..r
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn summary_worst_year_is_the_lowest_year() {
        let p = trending(10, 400);
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        let (from, to) = (p.date(260).unwrap(), p.date(399).unwrap());
        let s = backtest(&p, Some(&m), &config(3), from, to, None)
            .unwrap()
            .summary();
        // Rising prices every year: the worst year is positive, not 0.
        assert!(s.worst_year > 0.0, "{s:?}");
    }

    #[test]
    fn illiquid_stocks_are_skipped_for_large_capital() {
        let p = trending(10, 300);
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        let c = config(2);
        // Median daily value is about 1,000,000 per stock; 2% = 20,000.
        assert_eq!(select(&p, &m, &c, 299, Some(30_000.0), &[]).len(), 2);
        assert!(select(&p, &m, &c, 299, Some(100_000.0), &[]).is_empty());
    }

    #[test]
    fn plan_and_trades() {
        let p = trending(10, 300);
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        let targets = plan(&p, &m, &config(2), 299, 20_000.0, &[]).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].symbol, "S9");
        assert!(targets.iter().all(|t| t.rupees <= 10_000.0 && t.shares > 0));
        let current = vec![("S1".to_owned(), 5), (targets[0].symbol.clone(), 1)];
        let orders = trades(&targets, &current);
        assert_eq!(
            orders[0],
            Trade {
                symbol: "S1".into(),
                side: Side::Sell,
                shares: 5
            }
        );
        assert!(
            orders.iter().any(|t| t.symbol == "S9"
                && t.side == Side::Buy
                && t.shares == targets[0].shares - 1)
        );
        assert!(plan(&p, &m, &config(2), 299, 0.0, &[]).is_err());
    }
}
