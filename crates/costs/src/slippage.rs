//! How much worse than the reference price a fill is assumed to be.

use serde::{Deserialize, Serialize};
use trading_core::{Price, Side, TickRounding};

use crate::CostError;

/// Slippage per share on each fill: `pct` of the price, at least
/// `min_ticks` ticks, always a whole number of ticks.
///
/// Covers crossing the spread and moving the price a little. The defaults
/// (0.02%, at least one tick) suit liquid NSE stocks and small orders;
/// measure real fills against signal prices in paper trading and adjust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Slippage {
    pub pct: f64,
    pub min_ticks: u32,
}

impl Default for Slippage {
    fn default() -> Self {
        Self {
            pct: 0.02,
            min_ticks: 1,
        }
    }
}

impl Slippage {
    /// No slippage at all, for tests and for comparing against.
    pub const NONE: Slippage = Slippage {
        pct: 0.0,
        min_ticks: 0,
    };

    pub fn validate(&self) -> Result<(), CostError> {
        if !self.pct.is_finite() || !(0.0..=5.0).contains(&self.pct) {
            return Err(CostError::Config(format!(
                "slippage pct must be between 0 and 5, got {}",
                self.pct
            )));
        }
        if self.min_ticks > 1_000 {
            return Err(CostError::Config("slippage min_ticks above 1000".into()));
        }
        Ok(())
    }

    /// Slippage per share at `price`, rounded up to whole ticks.
    pub fn per_share(&self, price: Price, tick: Price) -> Price {
        let pct = Price::from_paise((price.paise() as f64 * self.pct / 100.0).ceil() as i64)
            .round_to_tick(tick, TickRounding::Up);
        let floor = Price::from_paise(tick.paise().saturating_mul(i64::from(self.min_ticks)));
        pct.max(floor)
    }

    /// The price a `side` fill is assumed to get when the market is at
    /// `price`: higher for buys, lower for sells (never below one tick).
    pub fn apply(&self, side: Side, price: Price, tick: Price) -> Price {
        let s = self.per_share(price, tick);
        match side {
            Side::Buy => price + s,
            Side::Sell => (price - s).max(tick),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(paise: i64) -> Price {
        Price::from_paise(paise)
    }

    #[test]
    fn per_share() {
        let s = Slippage::default();
        // ₹500 * 0.02% = 10 paise, two 5-paise ticks.
        assert_eq!(s.per_share(p(50_000), p(5)), p(10));
        // ₹100 * 0.02% = 2 paise, below the one-tick minimum of 5 paise.
        assert_eq!(s.per_share(p(10_000), p(5)), p(5));
        // ₹2,512.35 * 0.02% = 50.247 paise, rounded up to 55.
        assert_eq!(s.per_share(p(251_235), p(5)), p(55));
        // Penny tick: ₹120 * 0.02% = 2.4 paise, rounded up to 3.
        assert_eq!(s.per_share(p(12_000), p(1)), p(3));
        assert_eq!(Slippage::NONE.per_share(p(50_000), p(5)), p(0));
    }

    #[test]
    fn apply_is_against_us() {
        let s = Slippage::default();
        assert_eq!(s.apply(Side::Buy, p(50_000), p(5)), p(50_010));
        assert_eq!(s.apply(Side::Sell, p(50_000), p(5)), p(49_990));
        assert_eq!(s.apply(Side::Sell, p(5), p(5)), p(5));
    }

    #[test]
    fn rejects_bad_values() {
        assert!(
            Slippage {
                pct: -1.0,
                min_ticks: 1
            }
            .validate()
            .is_err()
        );
        assert!(
            Slippage {
                pct: f64::INFINITY,
                min_ticks: 1
            }
            .validate()
            .is_err()
        );
        Slippage::default().validate().unwrap();
    }
}
