//! Statutory charges and brokerage for NSE cash-market intraday (MIS)
//! trades.

use serde::{Deserialize, Serialize};
use trading_core::{Money, Price, Side};

use crate::CostError;

/// A rate as parts per billion of an amount, so charges are computed in
/// exact integer arithmetic (0.0030699% is exactly 30,699 ppb).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Rate(i64);

impl Rate {
    const ONE: i128 = 1_000_000_000;

    /// From a percentage, e.g. `0.025` for 0.025%.
    fn from_pct(pct: f64) -> Self {
        Self((pct * 1e7).round() as i64)
    }

    /// `amount * rate` in paise, rounded half up to the paisa.
    fn of(self, amount: i64) -> i64 {
        let x = i128::from(amount.max(0)) * i128::from(self.0);
        ((x + Self::ONE / 2) / Self::ONE) as i64
    }
}

/// Rounds paise half up to whole rupees.
fn to_rupee(paise: i64) -> i64 {
    (paise + 50) / 100 * 100
}

/// Charge rates, in percent of turnover unless stated. The defaults are
/// the NSE equity intraday rates shown on dhan.co/pricing when read in
/// November 2026. Rates change with budgets and exchange circulars: check
/// that page again before trading real money. Dhan's IPFT charge
/// (0.0000001%) is left out; it is under a paisa per order at our sizes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChargeRates {
    /// Brokerage per executed order: this percentage of the order value...
    pub brokerage_pct: f64,
    /// ...but at most this many rupees.
    pub brokerage_max: f64,
    /// Securities Transaction Tax, on the sell side only.
    pub stt_sell_pct: f64,
    /// Exchange transaction charges, both sides.
    pub exchange_pct: f64,
    /// SEBI turnover fee in rupees per crore of turnover, both sides.
    pub sebi_per_crore: f64,
    /// Stamp duty, on the buy side only.
    pub stamp_buy_pct: f64,
    /// GST, as a percentage of brokerage + exchange charges + SEBI fee.
    pub gst_pct: f64,
    /// Round STT and stamp duty to the nearest rupee, as Dhan's pricing
    /// page says it does. Rounding per order is our assumption.
    pub round_stt_stamp_to_rupee: bool,
}

impl Default for ChargeRates {
    fn default() -> Self {
        Self {
            brokerage_pct: 0.03,
            brokerage_max: 20.0,
            stt_sell_pct: 0.025,
            exchange_pct: 0.0030699,
            sebi_per_crore: 10.0,
            stamp_buy_pct: 0.003,
            gst_pct: 18.0,
            round_stt_stamp_to_rupee: true,
        }
    }
}

impl ChargeRates {
    pub fn validate(&self) -> Result<(), CostError> {
        let fail = |why: String| Err(CostError::Config(why));
        let fields = [
            ("brokerage_pct", self.brokerage_pct, 1.0),
            ("brokerage_max", self.brokerage_max, 1_000.0),
            ("stt_sell_pct", self.stt_sell_pct, 1.0),
            ("exchange_pct", self.exchange_pct, 1.0),
            ("sebi_per_crore", self.sebi_per_crore, 1_000.0),
            ("stamp_buy_pct", self.stamp_buy_pct, 1.0),
            ("gst_pct", self.gst_pct, 100.0),
        ];
        for (name, v, max) in fields {
            if !v.is_finite() || v < 0.0 || v > max {
                return fail(format!("{name} must be between 0 and {max}, got {v}"));
            }
        }
        Ok(())
    }

    pub(crate) fn compile(&self) -> Result<Compiled, CostError> {
        self.validate()?;
        Ok(Compiled {
            brokerage: Rate::from_pct(self.brokerage_pct),
            brokerage_max: Money::from_rupees_f64(self.brokerage_max)
                .map(Money::paise)
                .ok_or_else(|| CostError::Config("brokerage_max".into()))?,
            stt_sell: Rate::from_pct(self.stt_sell_pct),
            exchange: Rate::from_pct(self.exchange_pct),
            // ₹1 per crore is 1e-7 of turnover, i.e. 100 ppb.
            sebi: Rate((self.sebi_per_crore * 100.0).round() as i64),
            stamp_buy: Rate::from_pct(self.stamp_buy_pct),
            gst: Rate::from_pct(self.gst_pct),
            round_to_rupee: self.round_stt_stamp_to_rupee,
        })
    }
}

/// The charges on one executed order, each rounded to the paisa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Charges {
    pub brokerage: Money,
    pub stt: Money,
    pub exchange: Money,
    pub sebi: Money,
    pub stamp: Money,
    pub gst: Money,
}

impl Charges {
    pub fn total(&self) -> Money {
        self.brokerage + self.stt + self.exchange + self.sebi + self.stamp + self.gst
    }
}

impl std::ops::Add for Charges {
    type Output = Charges;
    fn add(self, o: Charges) -> Charges {
        Charges {
            brokerage: self.brokerage + o.brokerage,
            stt: self.stt + o.stt,
            exchange: self.exchange + o.exchange,
            sebi: self.sebi + o.sebi,
            stamp: self.stamp + o.stamp,
            gst: self.gst + o.gst,
        }
    }
}

/// Rates ready for integer arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Compiled {
    brokerage: Rate,
    brokerage_max: i64,
    stt_sell: Rate,
    exchange: Rate,
    sebi: Rate,
    stamp_buy: Rate,
    gst: Rate,
    round_to_rupee: bool,
}

impl Compiled {
    /// No charges at all.
    pub(crate) const ZERO: Compiled = Compiled {
        brokerage: Rate(0),
        brokerage_max: 0,
        stt_sell: Rate(0),
        exchange: Rate(0),
        sebi: Rate(0),
        stamp_buy: Rate(0),
        gst: Rate(0),
        round_to_rupee: false,
    };

    /// Charges on one executed order of `qty` shares at `price`.
    pub(crate) fn order(&self, side: Side, price: Price, qty: u32) -> Charges {
        let value = price.notional(qty).paise();
        let rupee = |p: i64| if self.round_to_rupee { to_rupee(p) } else { p };
        let brokerage = self.brokerage.of(value).min(self.brokerage_max);
        let exchange = self.exchange.of(value);
        let sebi = self.sebi.of(value);
        let (stt, stamp) = match side {
            Side::Buy => (0, rupee(self.stamp_buy.of(value))),
            Side::Sell => (rupee(self.stt_sell.of(value)), 0),
        };
        let gst = self.gst.of(brokerage + exchange + sebi);
        Charges {
            brokerage: Money::from_paise(brokerage),
            stt: Money::from_paise(stt),
            exchange: Money::from_paise(exchange),
            sebi: Money::from_paise(sebi),
            stamp: Money::from_paise(stamp),
            gst: Money::from_paise(gst),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_are_exact() {
        assert_eq!(Rate::from_pct(0.0030699).0, 30_699);
        assert_eq!(Rate::from_pct(18.0).0, 180_000_000);
        // 0.025% of ₹50,500 = ₹12.625, rounded half up to ₹12.63.
        assert_eq!(Rate::from_pct(0.025).of(5_050_000), 1_263);
        assert_eq!(to_rupee(1_263), 1_300);
        assert_eq!(to_rupee(150), 200);
        assert_eq!(to_rupee(149), 100);
    }

    #[test]
    fn rejects_bad_rates() {
        let bad = |f: fn(&mut ChargeRates)| {
            let mut r = ChargeRates::default();
            f(&mut r);
            r.compile().is_err()
        };
        assert!(bad(|r| r.stt_sell_pct = -0.1));
        assert!(bad(|r| r.gst_pct = f64::NAN));
        assert!(bad(|r| r.exchange_pct = 5.0));
        assert!(!bad(|r| r.brokerage_max = 0.0)); // zero brokerage is fine
    }
}
