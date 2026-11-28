//! Monthly stock rotation on daily bars.
//!
//! Each rebalance day, rank a point-in-time universe (the most-traded
//! stocks as of that day) with a [`Ranker`], hold the top few in equal
//! amounts from the next open, and repeat. The same code runs the
//! backtest ([`backtest`]), paper trades day by day ([`paper::Account`])
//! and produces a real month's orders ([`plan`] and [`trades`]), so they
//! cannot disagree.
//!
//! ```
//! use rotation::{RotationConfig, build};
//! let strategy = build("momentum_low_vol", serde_json::Value::Null).unwrap();
//! // Top 500 universe, hold 30, first trading day of each month, rank-60 exit buffer.
//! let config = RotationConfig::recommended();
//! # let _ = (strategy, config);
//! ```
//!
//! Strategies: `momentum` (default 12-1) and `momentum_low_vol`. See
//! `docs/research.md` for how they were found and tested.

mod panel;
pub mod paper;
mod rotation;
mod strategy;

pub use panel::{BREAK, Day, MAX_GAP_DAYS, MONTH, Panel, TRADING_DAYS};
pub use rotation::{
    Backtest, Period, RotationConfig, Schedule, Side, Summary, Target, Trade, UniverseConfig,
    backtest, excess, plan, ranking, select, trades, universe,
};
pub use strategy::{
    Momentum, MomentumConfig, MomentumLowVol, MomentumLowVolConfig, Ranker, STRATEGIES, build,
};

#[derive(Debug, thiserror::Error)]
pub enum RotationError {
    #[error("config: {0}")]
    Config(String),
    #[error("data: {0}")]
    Data(String),
}
