// Test support: failing loudly on a bad bar or signal is the point.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! A test bench for strategies: feed bars, set positions, read signals.
//!
//! ```
//! use strategies::testing::{Bench, day};
//!
//! let mut b = Bench::from_registry("template", serde_json::json!({"sma_period": 3}), 1).unwrap();
//! b.flat_day(0, day(2025, 1, 6), 100.0);            // warm-up
//! let out = b.bar(0, day(2025, 1, 7), 0, [99.0, 99.0, 98.0, 98.0], 1000);
//! assert!(out.is_empty());
//! ```
//!
//! Every signal is checked with [`Signal::validate`] before it is returned,
//! so a test fails as soon as a strategy produces something malformed.

use chrono::NaiveDate;
use indicators::{BankBuilder, IndicatorBank, Registry as Indicators};
use serde_json::Value;
use trading_core::{
    Bar, Exchange, InstrumentId, MarketHours, Money, Price, Signal, StrategyId, Universe,
};

use crate::{Context, PositionView, Registry, Strategy, StrategyError};

/// A date, for readable tests. Panics on an impossible date.
pub fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day)
        .unwrap_or_else(|| panic!("bad date {year}-{month}-{day}"))
}

/// One strategy over `n` test stocks named `S0`, `S1`, ... with a 5 paise
/// tick, and the indicators it asked for.
#[derive(Debug)]
pub struct Bench {
    strategy: Box<dyn Strategy>,
    bank: IndicatorBank,
    universe: Universe,
    positions: Vec<PositionView>,
    hours: MarketHours,
    /// Set to `false` to test behaviour on a broken feed.
    pub feed_ok: bool,
    /// Set to `false` to test a stock that may not be traded today.
    pub tradable: bool,
}

impl Bench {
    /// Builds the strategy with `build`, which receives the indicator
    /// builder and the number of stocks, as the engine does.
    pub fn new(
        stocks: u32,
        build: impl FnOnce(&mut BankBuilder, usize) -> Result<Box<dyn Strategy>, StrategyError>,
    ) -> Result<Self, StrategyError> {
        let mut universe = Universe::new();
        for i in 0..stocks {
            universe
                .add(
                    Exchange::Nse,
                    &format!("S{i}"),
                    1 + i,
                    Price::from_paise(5),
                    1,
                )
                .map_err(|e| StrategyError::Config {
                    strategy: "bench",
                    why: e.to_string(),
                })?;
        }
        let n = stocks as usize;
        let mut builder = BankBuilder::new(Indicators::builtin());
        let strategy = build(&mut builder, n)?;
        Ok(Self {
            strategy,
            bank: builder.build(n)?,
            universe,
            positions: vec![PositionView::FLAT; n],
            hours: MarketHours::NSE_EQUITY,
            feed_ok: true,
            tradable: true,
        })
    }

    /// Builds a registered strategy by name with JSON settings, exactly as a
    /// config file would.
    pub fn from_registry(name: &str, params: Value, stocks: u32) -> Result<Self, StrategyError> {
        Self::new(stocks, |builder, n| {
            Registry::builtin().build(name, StrategyId(0), params, builder, n)
        })
    }

    /// A bar for `stock` at `minute` after the open (0 = 09:15) on `date`,
    /// followed by the end of that minute. Returns every signal produced.
    pub fn bar(
        &mut self,
        stock: u32,
        date: NaiveDate,
        minute: i64,
        [open, high, low, close]: [f64; 4],
        volume: u64,
    ) -> Vec<Signal> {
        let start = self.hours.open_at(date).expect("valid session date");
        let price = |r: f64| Price::from_rupees_f64(r).expect("finite price");
        let bar = Bar {
            instrument: InstrumentId(stock),
            ts: start.plus_minutes(minute),
            open: price(open),
            high: price(high),
            low: price(low),
            close: price(close),
            volume,
        };
        bar.validate().expect("test bar must be valid");
        let session = self.bank.update(&bar).expect("bars in order");
        let ctx = Context {
            instrument: self.universe.get(bar.instrument).expect("known stock"),
            bank: &self.bank,
            session,
            hours: self.hours,
            feed_ok: self.feed_ok,
            tradable: self.tradable,
            position: self.positions[stock as usize],
        };
        let mut out = Vec::new();
        self.strategy.on_bar(&bar, &ctx, &mut out);
        self.strategy
            .on_minute_end(bar.ts.plus_minutes(1), &mut out);
        for s in &out {
            s.validate(s.entry.unwrap_or(bar.close))
                .unwrap_or_else(|e| panic!("invalid signal {s:?}: {e}"));
        }
        out
    }

    /// A whole session of flat bars at `price`, e.g. for warm-up days.
    pub fn flat_day(&mut self, stock: u32, date: NaiveDate, price: f64) -> Vec<Signal> {
        let minutes = i64::from(self.hours.minutes_per_session());
        (0..minutes)
            .flat_map(|m| self.bar(stock, date, m, [price; 4], 1000))
            .collect()
    }

    /// As if an order filled: the stock's position becomes `qty` shares
    /// (negative for short) at `price`.
    pub fn fill(&mut self, stock: u32, qty: i64, price: f64) {
        let p = &mut self.positions[stock as usize];
        p.qty = qty;
        p.avg_price = Price::from_rupees_f64(price);
    }

    /// As if the position closed with `realized` rupees of profit today.
    pub fn close_out(&mut self, stock: u32, realized: f64) {
        self.positions[stock as usize] = PositionView {
            qty: 0,
            avg_price: None,
            realized: Money::from_rupees_f64(realized).unwrap_or(Money::ZERO),
        };
    }

    /// The strategy's filter counts, as reports show them.
    pub fn filter_counts(&self) -> Vec<(&'static str, u64)> {
        self.strategy.filter_counts()
    }

    /// The indicator values the strategy sees.
    pub fn bank(&self) -> &IndicatorBank {
        &self.bank
    }
}
