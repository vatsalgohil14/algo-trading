//! Decides whether a signal becomes a trade, and how big.
//!
//! Strategies say what they want; [`RiskManager::evaluate`] says whether
//! they can have it. For an entry, in order:
//!
//! 1. Nothing new while trading is halted.
//! 2. One position per stock across all strategies, because the broker nets
//!    everything per stock.
//! 3. **Size**: `min_exposure` plus the setup's score times the rest of the
//!    range, in whole lots, shrunk so that hitting the stop loses at most
//!    `max_loss_per_trade`. A trade that would fall below `min_exposure` is
//!    skipped.
//! 4. Expected reward must be a multiple of the expected round-trip cost.
//! 5. Total exposure, net long or short exposure, and margin must stay
//!    within their limits.
//!
//! Exits are never blocked, even while halted. A stop can only be tightened.
//!
//! An approved entry reserves its exposure at once, so several signals in
//! the same minute cannot together pass a limit. The reservation is released
//! with [`RiskManager::release`] when the entry order is finished.
//!
//! All decisions use exact integer paise and no clock of their own, so the
//! same code gives the same results in backtest, paper and live trading.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod config;
mod manager;

pub use config::RiskConfig;
pub use manager::{Approved, Decision, Halt, RiskEvent, RiskManager, RiskReject};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RiskError {
    #[error("invalid risk config: {0}")]
    Config(String),

    #[error("cannot restore from the broker: {0}")]
    Restore(String),
}

pub(crate) fn check_leverage(leverage: f64) -> Result<(), RiskError> {
    if leverage.is_finite() && (1.0..=20.0).contains(&leverage) {
        Ok(())
    } else {
        Err(RiskError::Config(format!(
            "leverage must be from 1 to 20, got {leverage}"
        )))
    }
}

#[cfg(test)]
mod tests;
