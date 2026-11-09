//! Strategies: turn bars and indicator values into [`Signal`]s.
//!
//! A strategy never sizes or places orders. It says what it wants (enter
//! long here with this stop and target, move the stop, get out) and the
//! risk manager decides whether and how much. That keeps every strategy
//! small, testable bar by bar, and identical in backtest, paper and live.
//!
//! * One strategy instance (one type with one parameter set, one
//!   [`StrategyId`]) handles all the stocks it is assigned, keeping its
//!   per-stock state in `Vec`s indexed by [`InstrumentId`].
//! * Indicators are asked for by name when the strategy is built
//!   ([`indicators::BankBuilder::require`]) and read through the shared
//!   [`IndicatorBank`] on each bar.
//! * The strategy's position comes from the order manager on every bar
//!   ([`Context::position`]), which mirrors the broker. Strategies derive
//!   their state from it rather than assuming their orders filled.
//! * [`StrategySet`] holds the configured strategies and which stocks each
//!   one trades.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod error;
pub mod orb;
mod registry;
mod set;
pub mod template;

pub use error::StrategyError;
pub use registry::build;
pub use set::StrategySet;

use std::fmt::Debug;

use indicators::{BarContext, IndicatorBank};
use trading_core::{
    Bar, Instrument, InstrumentId, MarketHours, Money, Price, Signal, StrategyId, Timestamp,
};

/// This strategy's position in one stock, as the order manager sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionView {
    /// Shares held: positive long, negative short.
    pub qty: i64,
    /// Average entry price while not flat.
    pub avg_price: Option<Price>,
    /// Realized profit or loss in this stock today, before charges.
    pub realized: Money,
}

impl PositionView {
    pub const FLAT: PositionView = PositionView {
        qty: 0,
        avg_price: None,
        realized: Money::ZERO,
    };

    pub const fn is_flat(&self) -> bool {
        self.qty == 0
    }
}

/// Everything a strategy may look at besides the bar itself.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub instrument: &'a Instrument,
    /// Indicator values, already updated with this bar.
    pub bank: &'a IndicatorBank,
    pub session: BarContext,
    pub hours: MarketHours,
    /// Market data is flowing normally. When false, strategies must not
    /// open new positions (exits are still allowed).
    pub feed_ok: bool,
    /// The stock may be traded intraday today: allowed for MIS, not under
    /// surveillance, no corporate action, not near a circuit limit. Set
    /// before the open from exchange and broker lists.
    pub tradable: bool,
    pub position: PositionView,
}

/// A trading strategy.
pub trait Strategy: Send + Debug {
    fn id(&self) -> StrategyId;

    /// Short name for logs and reports, e.g. `"orb"`.
    fn name(&self) -> &'static str;

    /// Called with each completed 1-minute bar of a stock this strategy
    /// trades, after the indicators have taken it. Push any signals onto
    /// `out`; they are acted on from the next bar, never this one.
    fn on_bar(&mut self, bar: &Bar, ctx: &Context<'_>, out: &mut Vec<Signal>);

    /// Called once per minute, after the bars of every stock for the
    /// minute ending at `close` have been given to [`on_bar`](Self::on_bar)
    /// (or a short deadline has passed). For decisions that compare stocks,
    /// such as choosing among several breakouts when only some can be taken.
    fn on_minute_end(&mut self, close: Timestamp, out: &mut Vec<Signal>) {
        let _ = (close, out);
    }

    /// How often each of the strategy's filters stopped a trade, by name,
    /// for reports.
    fn filter_counts(&self) -> Vec<(&'static str, u64)> {
        Vec::new()
    }
}

/// Whether `instrument` is within a per-stock table of `len` entries.
pub(crate) fn in_range(instrument: InstrumentId, len: usize) -> bool {
    instrument.index() < len
}
