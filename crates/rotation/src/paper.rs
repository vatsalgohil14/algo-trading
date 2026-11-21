//! Paper trading the rotation one day at a time, with the same selection as
//! the backtest: decide on a rebalance day's close, trade at the next open,
//! pay costs on every trade, hold whole shares and real cash.
//!
//! An [`Account`] is plain data (save it as JSON between runs). Each run
//! calls [`Account::advance`] with a panel that reaches the latest day;
//! days already processed are skipped, so running twice changes nothing.

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::RotationError;
use crate::panel::Panel;
use crate::rotation::{RotationConfig, Side, select};
use crate::strategy::Ranker;

/// A pending order that cannot fill (no trading that day) is retried on the
/// next days, up to this many, then dropped until the next rebalance.
pub const MAX_FILL_ATTEMPTS: u32 = 5;
/// A held stock's earlier close moving by more than this between runs means
/// the data was adjusted for a split or bonus.
const ADJUSTMENT_THRESHOLD: f64 = 0.05;
/// At a rebalance, kept stocks are trimmed or topped up to an equal share
/// only when more than this far from it (25%), to avoid tiny trades.
pub const WEIGHT_BAND: f64 = 0.25;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub shares: u64,
    pub entry_date: NaiveDate,
    /// Rupees per share paid, before costs (adjusted with splits).
    pub entry_price: f64,
    /// The last day the stock traded, and its close then.
    pub last_date: NaiveDate,
    pub last_close: f64,
}

/// A rebalance decided on a close, to be traded at the next open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub decided: NaiveDate,
    /// Symbols to hold after the trades, best first.
    pub targets: Vec<String>,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fill {
    pub date: NaiveDate,
    pub symbol: String,
    pub side: Side,
    pub shares: u64,
    pub price: f64,
    pub value: f64,
    pub cost: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Event {
    Filled(Fill),
    /// Shares multiplied by `factor` (2.0 for a 1:1 bonus or a 2-for-1 split)
    /// because the price history was adjusted.
    Adjusted {
        symbol: String,
        factor: f64,
    },
    NotFilled {
        symbol: String,
        side: Side,
        reason: String,
    },
    /// New targets decided on this close.
    Rebalance {
        targets: Vec<String>,
    },
    /// Orders still unfilled after [`MAX_FILL_ATTEMPTS`] days.
    GaveUp {
        symbols: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayLog {
    pub date: NaiveDate,
    pub cash: f64,
    /// Cash plus holdings at the close.
    pub equity: f64,
    pub events: Vec<Event>,
}

/// What one [`Account::advance`] did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Progress {
    /// Split and bonus adjustments found in the data since the last run.
    pub adjustments: Vec<Event>,
    pub days: Vec<DayLog>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Strategy name and settings, as for [`crate::build`].
    pub strategy: String,
    pub params: serde_json::Value,
    pub config: RotationConfig,
    pub start_capital: f64,
    /// The first day traded: its close is the first rebalance.
    pub start: NaiveDate,
    pub cash: f64,
    pub positions: Vec<Position>,
    pub pending: Option<Pending>,
    pub last_day: Option<NaiveDate>,
    pub last_rebalance: Option<NaiveDate>,
    pub peak_equity: f64,
}

impl Account {
    pub fn new(
        strategy: &str,
        params: serde_json::Value,
        config: RotationConfig,
        capital: f64,
        start: NaiveDate,
    ) -> Result<Self, RotationError> {
        config.validate()?;
        if !(capital.is_finite() && capital > 0.0) {
            return Err(RotationError::Config("capital must be positive".into()));
        }
        Ok(Self {
            strategy: strategy.to_owned(),
            params,
            config,
            start_capital: capital,
            start,
            cash: capital,
            positions: Vec::new(),
            pending: None,
            last_day: None,
            last_rebalance: None,
            peak_equity: capital,
        })
    }

    /// Cash plus holdings at their last close.
    pub fn equity(&self) -> f64 {
        self.cash
            + self
                .positions
                .iter()
                .map(|x| x.shares as f64 * x.last_close)
                .sum::<f64>()
    }

    /// Fall from the highest equity so far, as a fraction.
    pub fn drawdown(&self) -> f64 {
        if self.peak_equity > 0.0 {
            (1.0 - self.equity() / self.peak_equity).max(0.0)
        } else {
            0.0
        }
    }

    /// Processes every day in `p` after the last one processed (or from
    /// `start`): fills pending orders at the open, values holdings at the
    /// close, and decides a rebalance on the schedule's days.
    pub fn advance(&mut self, p: &Panel, ranker: &dyn Ranker) -> Result<Progress, RotationError> {
        self.config.validate()?;
        let index: HashMap<&str, usize> = p
            .symbols()
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let mut progress = Progress {
            adjustments: self.adjust(p, &index),
            days: Vec::new(),
        };
        let first = match self.last_day {
            Some(last) => p.dates().iter().position(|d| *d > last),
            None => p.day_on_or_after(self.start),
        };
        let Some(first) = first else {
            return Ok(progress);
        };
        for d in first..p.dates().len() {
            progress.days.push(self.step(p, ranker, &index, d)?);
        }
        Ok(progress)
    }

    /// Rescales positions whose earlier close changed: the data provider
    /// adjusted the history for a split or bonus.
    fn adjust(&mut self, p: &Panel, index: &HashMap<&str, usize>) -> Vec<Event> {
        let mut events = Vec::new();
        for pos in &mut self.positions {
            let Some(&s) = index.get(pos.symbol.as_str()) else {
                continue;
            };
            let Some(bar) = p
                .dates()
                .iter()
                .position(|d| *d == pos.last_date)
                .and_then(|d| p.get(s, d))
            else {
                continue;
            };
            let ratio = bar.close / pos.last_close;
            if (ratio - 1.0).abs() <= ADJUSTMENT_THRESHOLD {
                continue;
            }
            let factor = 1.0 / ratio;
            pos.shares = (pos.shares as f64 * factor).round() as u64;
            pos.entry_price *= ratio;
            pos.last_close = bar.close;
            events.push(Event::Adjusted {
                symbol: pos.symbol.clone(),
                factor,
            });
        }
        events
    }

    fn step(
        &mut self,
        p: &Panel,
        ranker: &dyn Ranker,
        index: &HashMap<&str, usize>,
        d: usize,
    ) -> Result<DayLog, RotationError> {
        let date = p
            .date(d)
            .ok_or_else(|| RotationError::Data("day out of range".into()))?;
        let mut events = Vec::new();
        if let Some(pending) = self.pending.take() {
            if pending.decided < date {
                self.fill(p, index, d, date, pending, &mut events);
            } else {
                self.pending = Some(pending);
            }
        }
        for pos in &mut self.positions {
            if let Some(bar) = index.get(pos.symbol.as_str()).and_then(|&s| p.get(s, d)) {
                pos.last_close = bar.close;
                pos.last_date = date;
            }
        }
        let last = self.last_rebalance.and_then(|x| p.day_on_or_before(x));
        if self.config.schedule.due(p, d, last) {
            let held: Vec<usize> = self
                .positions
                .iter()
                .filter_map(|x| index.get(x.symbol.as_str()).copied())
                .collect();
            let targets: Vec<String> =
                select(p, ranker, &self.config, d, Some(self.equity()), &held)
                    .into_iter()
                    .filter_map(|s| p.symbol(s).map(str::to_owned))
                    .collect();
            events.push(Event::Rebalance {
                targets: targets.clone(),
            });
            self.pending = Some(Pending {
                decided: date,
                targets,
                attempts: 0,
            });
            self.last_rebalance = Some(date);
        }
        let equity = self.equity();
        self.peak_equity = self.peak_equity.max(equity);
        self.last_day = Some(date);
        Ok(DayLog {
            date,
            cash: self.cash,
            equity,
            events,
        })
    }

    /// Trades `pending` at day `d`'s open, back to equal weights like the
    /// backtest: sells what is no longer a target, trims kept stocks more
    /// than [`WEIGHT_BAND`] above an equal share, buys new targets with an
    /// equal share each, then tops up kept stocks more than the band below.
    fn fill(
        &mut self,
        p: &Panel,
        index: &HashMap<&str, usize>,
        d: usize,
        date: NaiveDate,
        mut pending: Pending,
        events: &mut Vec<Event>,
    ) {
        let fee = self.config.cost_bps / 2.0 / 10_000.0;
        let open = |symbol: &str| index.get(symbol).and_then(|&s| p.get(s, d)).map(|x| x.open);
        let targets: HashSet<&str> = pending.targets.iter().map(String::as_str).collect();
        let mut missed = Vec::new();
        let mut fill = |side: Side, symbol: &str, shares: u64, price: f64, cash: &mut f64| {
            let value = shares as f64 * price;
            let cost = value * fee;
            match side {
                Side::Sell => *cash += value - cost,
                Side::Buy => *cash -= value + cost,
            }
            events.push(Event::Filled(Fill {
                date,
                symbol: symbol.to_owned(),
                side,
                shares,
                price,
                value,
                cost,
            }));
        };
        let mut unfilled = Vec::new();
        let mut skipped = Vec::new();

        // 1. Exits.
        let mut kept = Vec::new();
        for pos in std::mem::take(&mut self.positions) {
            if targets.contains(pos.symbol.as_str()) {
                kept.push(pos);
            } else if let Some(price) = open(&pos.symbol) {
                fill(Side::Sell, &pos.symbol, pos.shares, price, &mut self.cash);
            } else {
                unfilled.push((pos.symbol.clone(), Side::Sell));
                kept.push(pos);
            }
        }
        self.positions = kept;

        let equity_at_open = self.cash
            + self
                .positions
                .iter()
                .map(|x| x.shares as f64 * open(&x.symbol).unwrap_or(x.last_close))
                .sum::<f64>();
        let per = equity_at_open / self.config.hold as f64;

        // 2. Trim kept stocks well above an equal share.
        for pos in &mut self.positions {
            let Some(price) = open(&pos.symbol) else {
                continue;
            };
            if targets.contains(pos.symbol.as_str())
                && pos.shares as f64 * price > per * (1.0 + WEIGHT_BAND)
            {
                let keep = (per / price).floor() as u64;
                let sell = pos.shares.saturating_sub(keep);
                if sell > 0 && keep > 0 {
                    fill(Side::Sell, &pos.symbol, sell, price, &mut self.cash);
                    pos.shares = keep;
                }
            }
        }

        // 3. New targets, sharing the cash equally if it runs short.
        let new: Vec<(&String, Option<f64>)> = pending
            .targets
            .iter()
            .filter(|t| !self.positions.iter().any(|x| &x.symbol == *t))
            .map(|t| (t, open(t)))
            .collect();
        let mut left = new.iter().filter(|x| x.1.is_some()).count();
        for (symbol, price) in new {
            let Some(price) = price else {
                unfilled.push((symbol.clone(), Side::Buy));
                continue;
            };
            let budget = per.min(self.cash.max(0.0) / (1.0 + fee) / left.max(1) as f64);
            left = left.saturating_sub(1);
            let shares = (budget / price).floor() as u64;
            if shares == 0 {
                skipped.push(symbol.clone());
                continue;
            }
            fill(Side::Buy, symbol, shares, price, &mut self.cash);
            self.positions.push(Position {
                symbol: symbol.clone(),
                shares,
                entry_date: date,
                entry_price: price,
                last_date: date,
                last_close: price,
            });
        }

        // 4. Top up kept stocks well below an equal share, with what is left.
        for pos in &mut self.positions {
            let Some(price) = open(&pos.symbol) else {
                continue;
            };
            let value = pos.shares as f64 * price;
            if pos.entry_date == date || value >= per * (1.0 - WEIGHT_BAND) {
                continue;
            }
            let budget = (per - value).min(self.cash.max(0.0) / (1.0 + fee));
            let add = (budget / price).floor() as u64;
            if add > 0 {
                fill(Side::Buy, &pos.symbol, add, price, &mut self.cash);
                pos.entry_price = (pos.entry_price * pos.shares as f64 + price * add as f64)
                    / (pos.shares + add) as f64;
                pos.shares += add;
            }
        }

        for symbol in skipped {
            events.push(Event::NotFilled {
                symbol,
                side: Side::Buy,
                reason: "an equal share is less than one share".into(),
            });
        }
        for (symbol, side) in unfilled {
            events.push(Event::NotFilled {
                symbol: symbol.clone(),
                side,
                reason: "no trading today".into(),
            });
            missed.push(symbol);
        }

        if missed.is_empty() {
            return;
        }
        pending.attempts += 1;
        if pending.attempts >= MAX_FILL_ATTEMPTS {
            events.push(Event::GaveUp { symbols: missed });
        } else {
            self.pending = Some(pending);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::panel::tests::panel;
    use crate::rotation::UniverseConfig;
    use crate::strategy::{Momentum, MomentumConfig};

    /// Stock i grows at i * 0.05% a day; `cut` drops the last days.
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

    fn account(p: &Panel, start_day: usize) -> Account {
        let config = RotationConfig {
            universe: UniverseConfig {
                size: 10,
                min_price: 1.0,
                ..UniverseConfig::default()
            },
            hold: 3,
            ..RotationConfig::default()
        };
        Account::new(
            "momentum",
            serde_json::Value::Null,
            config,
            30_000.0,
            p.date(start_day).unwrap(),
        )
        .unwrap()
    }

    fn momentum() -> Momentum {
        Momentum::new(MomentumConfig::default()).unwrap()
    }

    #[test]
    fn buys_the_targets_at_the_next_open_and_rebalances_monthly() {
        let p = trending(10, 420);
        let mut a = account(&p, 300);
        let progress = a.advance(&p, &momentum()).unwrap();
        assert_eq!(progress.days.len(), 120);
        // Decided on day 300's close, bought at day 301's open.
        assert!(matches!(
            &progress.days[0].events[..],
            [Event::Rebalance { .. }]
        ));
        let buys: Vec<&Fill> = progress.days[1]
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Filled(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(
            buys.iter().map(|f| f.symbol.as_str()).collect::<Vec<_>>(),
            ["S9", "S8", "S7"]
        );
        assert!(
            buys.iter()
                .all(|f| f.side == Side::Buy && f.date == p.date(301).unwrap())
        );
        let mut held: Vec<&str> = a.positions.iter().map(|x| x.symbol.as_str()).collect();
        held.sort_unstable();
        assert_eq!(held, ["S7", "S8", "S9"]);
        assert!(a.cash >= 0.0 && a.cash < 1_000.0);
        // One rebalance per calendar month after the first.
        let rebalances = progress
            .days
            .iter()
            .filter(|x| {
                x.events
                    .iter()
                    .any(|e| matches!(e, Event::Rebalance { .. }))
            })
            .count();
        assert!((4..=6).contains(&rebalances), "{rebalances}");
        assert!(a.equity() > 30_000.0);
        assert_eq!(a.drawdown(), 0.0);
    }

    #[test]
    fn running_in_pieces_equals_running_once_and_reruns_change_nothing() {
        let (short, long) = (trending(10, 350), trending(10, 420));
        let mut pieces = account(&long, 300);
        pieces.advance(&short, &momentum()).unwrap();
        pieces.advance(&long, &momentum()).unwrap();
        let mut once = account(&long, 300);
        once.advance(&long, &momentum()).unwrap();
        assert_eq!(pieces, once);
        let again = once.advance(&long, &momentum()).unwrap();
        assert!(again.days.is_empty());
        assert_eq!(pieces, once);
    }

    #[test]
    fn split_in_the_data_rescales_shares() {
        let p = trending(10, 320);
        let mut a = account(&p, 300);
        a.advance(&p, &momentum()).unwrap();
        // Pretend the stored close was twice the data's: a 2-for-1 split.
        let pos = &mut a.positions[0];
        let (shares, close) = (pos.shares, pos.last_close);
        pos.last_close = close * 2.0;
        pos.shares = shares / 2;
        let equity = a.equity();
        let progress = a.advance(&p, &momentum()).unwrap();
        assert!(matches!(
            &progress.adjustments[..],
            [Event::Adjusted { factor, .. }] if (*factor - 2.0).abs() < 1e-9
        ));
        assert_eq!(a.positions[0].shares, shares / 2 * 2);
        assert!((a.equity() - equity).abs() < 1e-6);
    }

    #[test]
    fn orders_for_a_stock_not_trading_wait_then_give_up() {
        // S9 stops trading right after the first rebalance day.
        let rows: Vec<Vec<Option<f64>>> = (0..10)
            .map(|i| {
                (0..320)
                    .map(|d| {
                        (i != 9 || d <= 300).then(|| 100.0 * (1.0 + i as f64 * 0.0005).powi(d))
                    })
                    .collect()
            })
            .collect();
        let refs: Vec<&[Option<f64>]> = rows.iter().map(Vec::as_slice).collect();
        let p = panel(&refs);
        let mut a = account(&p, 300);
        let progress = a.advance(&p, &momentum()).unwrap();
        let events: Vec<&Event> = progress.days.iter().flat_map(|x| &x.events).collect();
        let misses = events
            .iter()
            .filter(|e| matches!(e, Event::NotFilled { symbol, .. } if symbol == "S9"))
            .count();
        assert_eq!(misses, MAX_FILL_ATTEMPTS as usize);
        assert!(events.iter().any(|e| matches!(e, Event::GaveUp { .. })));
        // The next month's rebalance replaced it with the next best.
        assert_eq!(a.positions.len(), 3);
        assert!(a.positions.iter().all(|x| x.symbol != "S9"));
    }

    #[test]
    fn rejects_bad_capital() {
        let p = trending(2, 10);
        let c = RotationConfig::default();
        assert!(
            Account::new(
                "momentum",
                serde_json::Value::Null,
                c,
                0.0,
                p.date(0).unwrap()
            )
            .is_err()
        );
    }
}
