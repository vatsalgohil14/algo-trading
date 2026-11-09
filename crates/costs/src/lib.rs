//! What trading really costs: brokerage, statutory charges and slippage
//! for NSE cash-market intraday (MIS) trades.
//!
//! For many small intraday trades, costs decide whether a strategy makes
//! money, so every backtest and every risk check uses this one model.
//!
//! * **Charges** ([`ChargeRates`]): brokerage (percentage, capped per order),
//!   STT on sells, exchange transaction charges, SEBI fee, stamp duty on
//!   buys and GST. Computed per executed order in exact integer paise.
//!   The rates are from Dhan's pricing page; the results have not yet been
//!   compared with a real contract note.
//! * **Slippage** ([`Slippage`]): fills assumed a little worse than the
//!   reference price, at least one tick.
//!
//! ```
//! use costs::CostModel;
//! use trading_core::{Price, Side};
//!
//! let model = CostModel::default();
//! let buy = model.charges(Side::Buy, Price::from_paise(50_000), 100);
//! assert_eq!(buy.total().to_string(), "21.56");
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

mod rates;
mod slippage;

pub use rates::{ChargeRates, Charges};
pub use slippage::Slippage;

use serde::{Deserialize, Serialize};
use trading_core::{Money, Price, Side};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CostError {
    #[error("invalid cost config: {0}")]
    Config(String),
}

/// Settings for [`CostModel`], e.g. from a config file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CostConfig {
    pub charges: ChargeRates,
    pub slippage: Slippage,
}

/// The result of one completed trade (entry and exit of the same size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundTrip {
    /// Profit or loss from prices alone.
    pub gross: Money,
    /// Both orders' charges.
    pub charges: Charges,
    /// `gross` minus charges.
    pub net: Money,
}

/// Charges and slippage. Cheap to copy around; build it once at start-up.
#[derive(Debug, Clone)]
pub struct CostModel {
    rates: rates::Compiled,
    slippage: Slippage,
}

impl Default for CostModel {
    /// Default NSE intraday rates and slippage.
    fn default() -> Self {
        // The defaults are known to be valid; fall back to zero brokerage
        // only to satisfy the type system.
        Self::new(&CostConfig::default()).unwrap_or(Self {
            rates: rates::Compiled::ZERO,
            slippage: Slippage::NONE,
        })
    }
}

impl CostModel {
    /// Fails on rates or slippage that make no sense.
    pub fn new(config: &CostConfig) -> Result<Self, CostError> {
        config.slippage.validate()?;
        Ok(Self {
            rates: config.charges.compile()?,
            slippage: config.slippage.clone(),
        })
    }

    pub fn slippage(&self) -> &Slippage {
        &self.slippage
    }

    /// Charges on one executed order.
    pub fn charges(&self, side: Side, price: Price, qty: u32) -> Charges {
        self.rates.order(side, price, qty)
    }

    /// A completed trade: `qty` shares entered on `entry_side` at `entry`
    /// and closed at `exit`. Prices are actual fill prices, so slippage is
    /// already in them.
    pub fn round_trip(&self, entry_side: Side, qty: u32, entry: Price, exit: Price) -> RoundTrip {
        let per_share = (exit - entry).paise() * entry_side.sign();
        let gross = Money::from_paise(per_share.saturating_mul(i64::from(qty)));
        let charges =
            self.charges(entry_side, entry, qty) + self.charges(entry_side.opposite(), exit, qty);
        RoundTrip {
            gross,
            charges,
            net: gross - charges.total(),
        }
    }

    /// Expected cost of trading `qty` shares in and out around `price`:
    /// both orders' charges plus slippage on both fills. The risk manager
    /// compares a trade's expected reward against this before taking it.
    pub fn expected_round_trip_cost(&self, price: Price, qty: u32, tick: Price) -> Money {
        let charges = self.charges(Side::Buy, price, qty) + self.charges(Side::Sell, price, qty);
        let slip = self.slippage.per_share(price, tick).notional(qty);
        charges.total() + slip + slip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rupees(s: &str) -> Money {
        Money::from_rupees_f64(s.parse().unwrap()).unwrap()
    }

    fn p(paise: i64) -> Price {
        Price::from_paise(paise)
    }

    /// 100 shares bought at ₹500 and sold at ₹505, worked by hand from the
    /// default rates. The brokerage is below the ₹20 cap on both sides.
    #[test]
    fn small_trade_worked_by_hand() {
        let m = CostModel::default();
        let buy = m.charges(Side::Buy, p(50_000), 100);
        assert_eq!(
            buy,
            Charges {
                brokerage: rupees("15.00"), // 0.03% of 50,000
                stt: Money::ZERO,
                exchange: rupees("1.53"), // 1.53495
                sebi: rupees("0.05"),
                stamp: rupees("2.00"), // 1.50, rounded to the rupee
                gst: rupees("2.98"),   // 18% of 16.58
            }
        );
        let sell = m.charges(Side::Sell, p(50_500), 100);
        assert_eq!(
            sell,
            Charges {
                brokerage: rupees("15.15"),
                stt: rupees("13.00"),     // 12.625, rounded to the rupee
                exchange: rupees("1.55"), // 1.55030
                sebi: rupees("0.05"),
                stamp: Money::ZERO,
                gst: rupees("3.02"), // 18% of 16.75
            }
        );
        let t = m.round_trip(Side::Buy, 100, p(50_000), p(50_500));
        assert_eq!(t.gross, rupees("500"));
        assert_eq!(t.charges.total(), rupees("54.33"));
        assert_eq!(t.net, rupees("445.67"));
    }

    /// 1,000 shares at ₹2,000 → ₹2,010: brokerage hits the ₹20 cap.
    #[test]
    fn large_trade_hits_the_brokerage_cap() {
        let m = CostModel::default();
        let t = m.round_trip(Side::Buy, 1_000, p(200_000), p(201_000));
        let c = t.charges;
        assert_eq!(c.brokerage, rupees("40"));
        assert_eq!(c.stt, rupees("503")); // 502.50 on ₹20.1L
        assert_eq!(c.stamp, rupees("60"));
        assert_eq!(c.exchange, rupees("123.10")); // 61.40 + 61.70
        assert_eq!(c.sebi, rupees("4.01"));
        assert_eq!(c.gst, rupees("30.08")); // 15.01 + 15.07
        assert_eq!(c.total(), rupees("760.19"));
        assert_eq!(t.net, rupees("9239.81"));
    }

    #[test]
    fn short_trades_pay_stt_on_the_entry() {
        let m = CostModel::default();
        // Sell 100 at ₹505 first, buy back at ₹500: same orders as a long
        // trade from 500 to 505, so the same charges and the same profit.
        let short = m.round_trip(Side::Sell, 100, p(50_500), p(50_000));
        let long = m.round_trip(Side::Buy, 100, p(50_000), p(50_500));
        assert_eq!(short, long);
        let loser = m.round_trip(Side::Sell, 100, p(50_000), p(50_500));
        assert_eq!(loser.gross, rupees("-500"));
        assert_eq!(loser.net, rupees("-554.33"));
    }

    #[test]
    fn expected_cost_includes_slippage() {
        let m = CostModel::default();
        // Charges for buy + sell at ₹500 (21.56 + 32.56 = 54.12) plus 10
        // paise of slippage on each side for 100 shares (₹20).
        let sell = m.charges(Side::Sell, p(50_000), 100).total();
        assert_eq!(sell, rupees("32.56"));
        assert_eq!(
            m.expected_round_trip_cost(p(50_000), 100, p(5)),
            rupees("74.12")
        );
    }

    #[test]
    fn zero_quantity_costs_nothing() {
        let m = CostModel::default();
        assert_eq!(m.charges(Side::Sell, p(50_000), 0).total(), Money::ZERO);
    }

    #[test]
    fn config_from_json() {
        let c: CostConfig =
            serde_json::from_str(r#"{"charges": {"brokerage_max": 0}, "slippage": {"pct": 0.05}}"#)
                .unwrap();
        let m = CostModel::new(&c).unwrap();
        assert_eq!(m.charges(Side::Buy, p(50_000), 100).brokerage, Money::ZERO);
        assert_eq!(m.slippage().pct, 0.05);
        assert!(serde_json::from_str::<CostConfig>(r#"{"brokerage": 1}"#).is_err());
        let bad = CostConfig {
            slippage: Slippage {
                pct: -1.0,
                min_ticks: 0,
            },
            ..CostConfig::default()
        };
        assert!(CostModel::new(&bad).is_err());
    }
}
