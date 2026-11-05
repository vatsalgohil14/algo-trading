//! Incremental technical indicators.
//!
//! * Every indicator updates in O(1) per bar ([`Indicator::update`]); nothing
//!   is recomputed over history.
//! * Indicators are named by a short spec string such as `"ema(20)"`,
//!   `"atr(14)"` or `"orb(15).high"` (see [`Spec`]), so strategies and config
//!   files can ask for them without code changes.
//! * An [`IndicatorBank`] holds one instance per (stock, indicator). If ten
//!   strategies ask for `ema(20)`, it is still computed once per stock.
//!
//! # Adding an indicator
//!
//! 1. Write a struct implementing [`Indicator`] in the folder that fits
//!    (`trend`, `momentum`, `volatility`, `volume`, `levels`).
//! 2. Add one line for it in [`Registry::builtin`].
//!
//! It can then be used from any strategy or config file by name.
//!
//! ```
//! use indicators::{BankBuilder, Registry};
//!
//! let mut builder = BankBuilder::new(Registry::builtin());
//! let ema = builder.require("ema(20)")?;
//! let orb_high = builder.require("orb(15).high")?;
//! let mut bank = builder.build(500)?; // 500 stocks
//! // per bar: bank.update(&bar)?; let v = bank.get(bar.instrument, ema);
//! # Ok::<(), indicators::IndicatorError>(())
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod bank;
mod error;
pub mod levels;
pub mod momentum;
mod registry;
mod spec;
pub mod trend;
pub mod volatility;
pub mod volume;

pub use bank::{BankBuilder, Handle, IndicatorBank};
pub use error::IndicatorError;
pub use registry::{Factory, Registry};
pub use spec::Spec;

use std::fmt::Debug;

use trading_core::Bar;

/// Where a bar sits in the trading day. Computed once per bar by the
/// [`IndicatorBank`] and shared by all indicators of that stock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarContext {
    /// First bar of a new IST trading day for this stock.
    pub new_session: bool,
    /// Minutes since the open (`0` for the 09:15 bar), `None` outside the
    /// session.
    pub session_minute: Option<u16>,
}

/// One indicator for one stock.
pub trait Indicator: Send + Debug {
    /// Names of the values this indicator produces. The first is the
    /// default, used when a spec names no output.
    fn outputs(&self) -> &'static [&'static str] {
        &["value"]
    }

    /// Takes the next bar. Bars arrive in time order, already validated.
    fn update(&mut self, bar: &Bar, ctx: &BarContext);

    /// Current value of output `index`, or `None` while warming up (or if
    /// the value is undefined, e.g. VWAP before any volume).
    fn value(&self, index: usize) -> Option<f64>;
}
