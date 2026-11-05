//! Net position in one instrument, with exact average-cost P&L.

use crate::{Fill, InstrumentId, Money, Price};

/// Net position built from fills.
///
/// `cost` is the signed cost basis of the open quantity (positive when long,
/// negative when short). When part of a position is closed, the matching
/// share of the cost basis is released with integer division; any remainder
/// of a paisa stays in `cost` and is released when the position goes flat,
/// so realised P&L over a round trip is always exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub instrument: InstrumentId,
    /// Shares; positive long, negative short.
    pub qty: i64,
    pub cost: Money,
    /// Realised P&L before charges.
    pub realized: Money,
    /// Total value traded, both sides (for charges and turnover limits).
    pub turnover: Money,
}

impl Position {
    pub const fn flat(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            qty: 0,
            cost: Money::ZERO,
            realized: Money::ZERO,
            turnover: Money::ZERO,
        }
    }

    pub const fn is_flat(&self) -> bool {
        self.qty == 0
    }

    /// Applies a fill and returns the P&L it realised. Fills for another
    /// instrument are ignored and realise nothing.
    pub fn apply(&mut self, fill: &Fill) -> Money {
        if fill.instrument != self.instrument || fill.qty == 0 {
            return Money::ZERO;
        }
        self.turnover += fill.price.notional(fill.qty);

        let signed = fill.side.sign() * i64::from(fill.qty);
        let price = i128::from(fill.price.paise());
        let mut remaining = signed;
        let mut realized: i128 = 0;

        // Closing part: fill against the open position, opposite direction.
        if self.qty != 0 && self.qty.signum() != signed.signum() {
            let closing = remaining.abs().min(self.qty.abs());
            let open_abs = i128::from(self.qty.abs());
            let cost = i128::from(self.cost.paise());
            let released = if i128::from(closing) == open_abs {
                cost
            } else {
                cost * i128::from(closing) / open_abs
            };
            // Long: proceeds - cost; short: cost (negative) - buyback.
            realized = i128::from(self.qty.signum()) * price * i128::from(closing) - released;
            self.cost = Money::from_paise(clamp(cost - released));
            self.qty -= self.qty.signum() * closing;
            remaining = signed - signed.signum() * closing;
        }

        // Opening part: whatever is left adds to (or starts) the position.
        if remaining != 0 {
            let cost = i128::from(self.cost.paise()) + price * i128::from(remaining);
            self.cost = Money::from_paise(clamp(cost));
            self.qty += remaining;
        }

        let realized = Money::from_paise(clamp(realized));
        self.realized += realized;
        realized
    }

    /// Average entry price of the open quantity, rounded to the nearest paisa.
    pub fn avg_price(&self) -> Option<Price> {
        if self.qty == 0 {
            return None;
        }
        let cost = i128::from(self.cost.paise());
        let qty = i128::from(self.qty);
        let avg = (2 * cost + qty) / (2 * qty); // round half up, signs agree
        Some(Price::from_paise(clamp(avg)))
    }

    /// Unrealised P&L if closed at `mark`.
    pub fn unrealized(&self, mark: Price) -> Money {
        let value = i128::from(mark.paise()) * i128::from(self.qty);
        Money::from_paise(clamp(value - i128::from(self.cost.paise())))
    }

    /// Absolute exposure at `mark`.
    pub fn exposure(&self, mark: Price) -> Money {
        Money::from_paise(clamp(i128::from(mark.paise()) * i128::from(self.qty.abs())))
    }
}

fn clamp(v: i128) -> i64 {
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientOrderId, Side, Timestamp};
    use std::sync::Arc;

    fn fill(side: Side, qty: u32, paise: i64) -> Fill {
        Fill {
            client_id: ClientOrderId::new("x").unwrap(),
            instrument: InstrumentId(0),
            side,
            qty,
            price: Price::from_paise(paise),
            ts: Timestamp::from_secs(0),
            trade_id: Arc::from("t"),
        }
    }

    fn m(paise: i64) -> Money {
        Money::from_paise(paise)
    }

    #[test]
    fn long_round_trip() {
        let mut p = Position::flat(InstrumentId(0));
        assert_eq!(p.apply(&fill(Side::Buy, 10, 10_000)), m(0));
        assert_eq!(p.avg_price(), Some(Price::from_paise(10_000)));
        assert_eq!(p.unrealized(Price::from_paise(10_500)), m(5_000));
        assert_eq!(p.apply(&fill(Side::Sell, 10, 11_000)), m(10_000));
        assert!(p.is_flat());
        assert_eq!(p.cost, m(0));
        assert_eq!(p.realized, m(10_000));
        assert_eq!(p.turnover, m(210_000));
    }

    #[test]
    fn short_round_trip() {
        let mut p = Position::flat(InstrumentId(0));
        p.apply(&fill(Side::Sell, 5, 20_000));
        assert_eq!(p.qty, -5);
        assert_eq!(p.avg_price(), Some(Price::from_paise(20_000)));
        assert_eq!(p.unrealized(Price::from_paise(19_000)), m(5_000));
        assert_eq!(p.apply(&fill(Side::Buy, 5, 19_000)), m(5_000));
        assert!(p.is_flat());
    }

    #[test]
    fn partial_closes_are_exact_overall() {
        // Three buys at different prices, closed in awkward pieces: the sum of
        // realised P&L must equal proceeds minus cost exactly.
        let mut p = Position::flat(InstrumentId(0));
        p.apply(&fill(Side::Buy, 1, 10_001));
        p.apply(&fill(Side::Buy, 1, 10_002));
        p.apply(&fill(Side::Buy, 1, 10_004));
        let mut total = m(0);
        total += p.apply(&fill(Side::Sell, 1, 10_100));
        total += p.apply(&fill(Side::Sell, 1, 10_100));
        total += p.apply(&fill(Side::Sell, 1, 10_100));
        assert_eq!(total, m(3 * 10_100 - (10_001 + 10_002 + 10_004)));
        assert_eq!(p.cost, m(0));
    }

    #[test]
    fn flip_from_long_to_short() {
        let mut p = Position::flat(InstrumentId(0));
        p.apply(&fill(Side::Buy, 10, 10_000));
        let r = p.apply(&fill(Side::Sell, 15, 10_200));
        assert_eq!(r, m(2_000));
        assert_eq!(p.qty, -5);
        assert_eq!(p.avg_price(), Some(Price::from_paise(10_200)));
        assert_eq!(p.cost, m(-51_000));
    }

    #[test]
    fn ignores_other_instruments() {
        let mut p = Position::flat(InstrumentId(1));
        assert_eq!(p.apply(&fill(Side::Buy, 10, 10_000)), m(0));
        assert!(p.is_flat());
        assert_eq!(p.turnover, m(0));
    }
}
