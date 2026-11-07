//! Runs ORB over months of synthetic data for many stocks with a minimal
//! fill model, checking the rules hold on every bar: signals are valid,
//! entries only in the window and once per stock and day, and every
//! position is closed by the end of the day.
//!
//! The fill model here is deliberately crude (fixed quantity, no costs);
//! it only exists to drive the strategy through fills, stops and exits.
//! Synthetic data has no edge, so the P&L means nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashSet;

use chrono::NaiveDate;
use indicators::{BankBuilder, Registry};
use sim::synthetic::{SyntheticConfig, SyntheticMarket, sample_universe, weekdays};
use strategies::orb::{Orb, OrbConfig};
use strategies::{Context, PositionView, Strategy};
use trading_core::{
    Bar, Exchange, Intent, MarketHours, MinuteOfDay, Money, Price, Signal, StrategyId, Universe,
};

const STOCKS: u32 = 40;
const QTY: i64 = 10;

#[derive(Debug, Clone, Copy, Default)]
struct Book {
    qty: i64,
    avg: Price,
    realized: Money,
    /// Working entry: (long, limit, stop, target).
    entry: Option<(bool, Price, Price, Price)>,
    stop: Price,
    target: Price,
    exit: bool,
}

impl Book {
    fn view(&self) -> PositionView {
        PositionView {
            qty: self.qty,
            avg_price: (self.qty != 0).then_some(self.avg),
            realized: self.realized,
        }
    }

    fn close_at(&mut self, price: Price) {
        let pnl = (price - self.avg).paise() * self.qty;
        self.realized += Money::from_paise(pnl);
        self.qty = 0;
    }

    /// Orders from earlier signals meet this bar.
    fn on_bar(&mut self, bar: &Bar, trades: &mut u32) {
        if self.exit {
            self.exit = false;
            self.entry = None;
            if self.qty != 0 {
                self.close_at(bar.open);
            }
            return;
        }
        if let Some((long, limit, stop, target)) = self.entry {
            let fill = if long {
                (bar.low <= limit).then(|| bar.open.min(limit))
            } else {
                (bar.high >= limit).then(|| bar.open.max(limit))
            };
            if let Some(price) = fill {
                self.entry = None;
                self.qty = if long { QTY } else { -QTY };
                self.avg = price;
                self.stop = stop;
                self.target = target;
                *trades += 1;
            }
            return; // stops and targets from the next bar
        }
        if self.qty > 0 {
            if bar.low <= self.stop {
                self.close_at(bar.open.min(self.stop));
            } else if bar.high >= self.target {
                self.close_at(bar.open.max(self.target));
            }
        } else if self.qty < 0 {
            if bar.high >= self.stop {
                self.close_at(bar.open.max(self.stop));
            } else if bar.low <= self.target {
                self.close_at(bar.open.min(self.target));
            }
        }
    }

    fn apply(&mut self, s: &Signal) {
        match s.intent {
            Intent::EnterLong | Intent::EnterShort => {
                self.entry = Some((
                    s.intent == Intent::EnterLong,
                    s.entry.unwrap(),
                    s.stop.unwrap(),
                    s.target.unwrap(),
                ));
            }
            Intent::AdjustStop => self.stop = s.stop.unwrap(),
            Intent::Exit => self.exit = true,
        }
    }
}

#[derive(Debug, PartialEq)]
struct Summary {
    entries: u32,
    fills: u32,
    exits_by_reason: Vec<(&'static str, u32)>,
    realized: Money,
}

fn run() -> (Summary, Vec<(&'static str, u64)>) {
    let profiles = sample_universe(STOCKS, 11);
    let mut universe = Universe::new();
    for p in &profiles {
        universe
            .add(Exchange::Nse, &p.symbol, p.instrument.0, p.tick_size, 1)
            .unwrap();
    }
    let mut market = SyntheticMarket::new(SyntheticConfig::default(), profiles).unwrap();

    let mut builder = BankBuilder::new(Registry::builtin());
    let config = OrbConfig {
        // Synthetic stocks are less liquid than the real target universe.
        min_turnover_cr: 5.0,
        ..OrbConfig::default()
    };
    let mut orb = Orb::new(StrategyId(0), config, &mut builder, STOCKS as usize).unwrap();
    let mut bank = builder.build(STOCKS as usize).unwrap();
    let hours = MarketHours::NSE_EQUITY;

    let mut books = vec![Book::default(); STOCKS as usize];
    let mut entries = 0;
    let mut fills = 0;
    let mut exits: Vec<(&'static str, u32)> = Vec::new();
    let mut out = Vec::new();
    let from = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let to = NaiveDate::from_ymd_opt(2025, 6, 30).unwrap();
    let window = (
        MinuteOfDay::new(9, 31).unwrap(),
        MinuteOfDay::new(11, 30).unwrap(),
    );

    for date in weekdays(from, to) {
        let mut entered_today = HashSet::new();
        for bar in market.session(date).unwrap() {
            let book = &mut books[bar.instrument.index()];
            book.on_bar(&bar, &mut fills);
            let session = bank.update(&bar).unwrap();
            let ctx = Context {
                instrument: universe.get(bar.instrument).unwrap(),
                bank: &bank,
                session,
                hours,
                feed_ok: true,
                tradable: true,
                position: book.view(),
            };
            out.clear();
            orb.on_bar(&bar, &ctx, &mut out);
            for s in &out {
                s.validate(bar.close).unwrap();
                assert_eq!(s.instrument, bar.instrument);
                match s.intent {
                    Intent::EnterLong | Intent::EnterShort => {
                        let t = s.ts.ist_minute_of_day();
                        assert!(t >= window.0 && t <= window.1, "entry at {t}");
                        assert!(
                            entered_today.insert(s.instrument),
                            "second entry in {} on {date}",
                            s.instrument
                        );
                        assert_eq!(book.qty, 0, "entry while holding a position");
                        entries += 1;
                    }
                    Intent::Exit => match exits.iter_mut().find(|(r, _)| *r == s.reason) {
                        Some((_, n)) => *n += 1,
                        None => exits.push((s.reason, 1)),
                    },
                    Intent::AdjustStop => {
                        let favour = if book.qty > 0 {
                            s.stop.unwrap() > book.stop
                        } else {
                            s.stop.unwrap() < book.stop
                        };
                        assert!(book.qty != 0 && favour, "stop moved the wrong way");
                    }
                }
                book.apply(s);
            }
        }
        for (i, book) in books.iter().enumerate() {
            assert_eq!(book.qty, 0, "stock {i} still open at the end of {date}");
        }
        assert!(orb.entries_today() <= 8);
    }
    exits.sort();
    let realized = books.iter().map(|b| b.realized).sum();
    let summary = Summary {
        entries,
        fills,
        exits_by_reason: exits,
        realized,
    };
    (summary, orb.rejections().collect())
}

#[test]
fn orb_follows_its_rules_on_six_months_of_synthetic_data() {
    let (summary, rejections) = run();
    println!("{summary:#?}\nrejections: {rejections:?}");
    assert!(summary.entries > 50, "too few entries to be a useful test");
    assert!(summary.fills > 0 && summary.fills <= summary.entries);
    let reasons: Vec<_> = summary.exits_by_reason.iter().map(|(r, _)| *r).collect();
    for expected in ["failed_breakout", "square_off"] {
        assert!(
            reasons.contains(&expected),
            "no {expected} exits in {reasons:?}"
        );
    }
    // Every filter is reachable on realistic data.
    for name in [
        "range_too_narrow",
        "range_too_wide",
        "volume",
        "vwap",
        "chase",
    ] {
        let n = rejections.iter().find(|(r, _)| *r == name).unwrap().1;
        assert!(n > 0, "filter {name} never fired");
    }

    // Same input, same output.
    assert_eq!(run().0, summary);
}
