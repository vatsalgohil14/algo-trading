//! Prices and amounts as integer paise (1 rupee = 100 paise).
//!
//! NSE tick sizes are whole paise (0.01 or 0.05), so every tradable price
//! fits exactly. Integers make arithmetic exact, cheap and deterministic;
//! `Decimal` is used only when talking to brokers and the database.
//! Arithmetic saturates instead of wrapping: realistic values are many orders
//! of magnitude below the limits, and a pinned value is easier to spot than a
//! silently wrapped one.

use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};

/// Largest magnitude accepted from floats or decimals: ₹10 lakh crore.
const MAX_PAISE: i64 = 1_000_000_000_000_000;

fn paise_from_f64(rupees: f64) -> Option<i64> {
    if !rupees.is_finite() {
        return None;
    }
    let p = (rupees * 100.0).round();
    if p.abs() > MAX_PAISE as f64 {
        return None;
    }
    Some(p as i64)
}

fn paise_from_decimal(rupees: Decimal) -> Option<i64> {
    let p = (rupees * Decimal::ONE_HUNDRED)
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_i64()?;
    (p.abs() <= MAX_PAISE).then_some(p)
}

fn write_paise(f: &mut fmt::Formatter<'_>, paise: i64) -> fmt::Result {
    let sign = if paise < 0 { "-" } else { "" };
    let abs = paise.unsigned_abs();
    write!(f, "{sign}{}.{:02}", abs / 100, abs % 100)
}

/// A per-share price in paise.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Price(i64);

/// Direction for [`Price::round_to_tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickRounding {
    /// Nearest tick; exact halves go up.
    Nearest,
    /// Largest tick at or below the price (e.g. a buy limit you will not exceed).
    Down,
    /// Smallest tick at or above the price.
    Up,
}

impl Price {
    pub const ZERO: Price = Price(0);

    pub const fn from_paise(paise: i64) -> Self {
        Self(paise)
    }

    pub const fn paise(self) -> i64 {
        self.0
    }

    /// From rupees, rounded to the nearest paisa. `None` for NaN, infinity or
    /// absurd magnitudes.
    pub fn from_rupees_f64(rupees: f64) -> Option<Self> {
        paise_from_f64(rupees).map(Self)
    }

    /// From rupees, rounded half away from zero to the nearest paisa.
    pub fn from_decimal(rupees: Decimal) -> Option<Self> {
        paise_from_decimal(rupees).map(Self)
    }

    pub fn to_decimal(self) -> Decimal {
        Decimal::new(self.0, 2)
    }

    /// Rupees as a float, for indicators and statistics only.
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / 100.0
    }

    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// Whether the price is a whole number of ticks. A non-positive tick
    /// size is never satisfied.
    pub const fn is_on_tick(self, tick: Price) -> bool {
        tick.0 > 0 && self.0 % tick.0 == 0
    }

    /// Rounds to a multiple of `tick`. Returns the price unchanged if `tick`
    /// is not positive.
    pub fn round_to_tick(self, tick: Price, mode: TickRounding) -> Price {
        let t = tick.0;
        if t <= 0 {
            return self;
        }
        let down = self.0.div_euclid(t) * t;
        let rem = self.0 - down;
        let rounded = match mode {
            TickRounding::Down => down,
            TickRounding::Up if rem == 0 => down,
            TickRounding::Up => down.saturating_add(t),
            TickRounding::Nearest if rem * 2 >= t => down.saturating_add(t),
            TickRounding::Nearest => down,
        };
        Price(rounded)
    }

    /// Value of `qty` shares at this price.
    pub const fn notional(self, qty: u32) -> Money {
        Money(self.0.saturating_mul(qty as i64))
    }

    /// `self` moved by a fraction of itself, e.g. `pct(-0.5)` is 0.5% lower.
    /// Not rounded to a tick.
    pub fn offset_pct(self, pct: f64) -> Option<Price> {
        paise_from_f64(self.as_f64() * (1.0 + pct / 100.0)).map(Price)
    }
}

impl Add for Price {
    type Output = Price;
    fn add(self, rhs: Price) -> Price {
        Price(self.0.saturating_add(rhs.0))
    }
}

impl Sub for Price {
    type Output = Price;
    fn sub(self, rhs: Price) -> Price {
        Price(self.0.saturating_sub(rhs.0))
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_paise(f, self.0)
    }
}

impl fmt::Debug for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Price(")?;
        write_paise(f, self.0)?;
        write!(f, ")")
    }
}

/// A signed amount of money in paise: P&L, charges, cost basis, cash.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);

    pub const fn from_paise(paise: i64) -> Self {
        Self(paise)
    }

    pub const fn paise(self) -> i64 {
        self.0
    }

    pub fn from_rupees_f64(rupees: f64) -> Option<Self> {
        paise_from_f64(rupees).map(Self)
    }

    pub fn from_decimal(rupees: Decimal) -> Option<Self> {
        paise_from_decimal(rupees).map(Self)
    }

    pub fn to_decimal(self) -> Decimal {
        Decimal::new(self.0, 2)
    }

    pub fn as_f64(self) -> f64 {
        self.0 as f64 / 100.0
    }

    pub const fn abs(self) -> Money {
        Money(self.0.saturating_abs())
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0.saturating_add(rhs.0))
    }
}

impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0.saturating_sub(rhs.0))
    }
}

impl AddAssign for Money {
    fn add_assign(&mut self, rhs: Money) {
        *self = *self + rhs;
    }
}

impl SubAssign for Money {
    fn sub_assign(&mut self, rhs: Money) {
        *self = *self - rhs;
    }
}

impl Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(self.0.saturating_neg())
    }
}

impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, Add::add)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_paise(f, self.0)
    }
}

impl fmt::Debug for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Money(")?;
        write_paise(f, self.0)?;
        write!(f, ")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn p(paise: i64) -> Price {
        Price::from_paise(paise)
    }

    #[test]
    fn conversions_round_to_the_paisa() {
        assert_eq!(Price::from_rupees_f64(1428.055), Some(p(142_806)));
        assert_eq!(Price::from_rupees_f64(0.1 + 0.2), Some(p(30)));
        assert_eq!(Price::from_rupees_f64(f64::NAN), None);
        assert_eq!(Price::from_rupees_f64(1e20), None);
        let d = Decimal::from_str("1428.055").unwrap();
        assert_eq!(Price::from_decimal(d), Some(p(142_806)));
        assert_eq!(p(142_806).to_decimal().to_string(), "1428.06");
        assert_eq!(
            Money::from_decimal(Decimal::from_str("-0.005").unwrap()),
            Some(Money::from_paise(-1))
        );
    }

    #[test]
    fn display() {
        assert_eq!(p(150_005).to_string(), "1500.05");
        assert_eq!(p(5).to_string(), "0.05");
        assert_eq!(Money::from_paise(-1_234).to_string(), "-12.34");
        assert_eq!(format!("{:?}", p(100)), "Price(1.00)");
    }

    #[test]
    fn tick_rounding() {
        let tick = p(5);
        assert_eq!(
            p(10_003).round_to_tick(tick, TickRounding::Nearest),
            p(10_005)
        );
        assert_eq!(
            p(10_002).round_to_tick(tick, TickRounding::Nearest),
            p(10_000)
        );
        assert_eq!(p(10_003).round_to_tick(tick, TickRounding::Down), p(10_000));
        assert_eq!(p(10_001).round_to_tick(tick, TickRounding::Up), p(10_005));
        assert_eq!(p(10_005).round_to_tick(tick, TickRounding::Up), p(10_005));
        assert_eq!(
            p(10_003).round_to_tick(Price::ZERO, TickRounding::Up),
            p(10_003)
        );
        assert!(p(10_005).is_on_tick(tick));
        assert!(!p(10_003).is_on_tick(tick));
        assert!(!p(10_000).is_on_tick(Price::ZERO));
    }

    #[test]
    fn arithmetic_saturates() {
        assert_eq!(p(i64::MAX) + p(1), p(i64::MAX));
        assert_eq!(p(10_000).notional(50), Money::from_paise(500_000));
        assert_eq!(p(i64::MAX).notional(2), Money::from_paise(i64::MAX));
        assert_eq!(-Money::from_paise(i64::MIN), Money::from_paise(i64::MAX));
        let total: Money = [1, 2, 3].into_iter().map(Money::from_paise).sum();
        assert_eq!(total, Money::from_paise(6));
    }

    #[test]
    fn offset_pct() {
        assert_eq!(p(100_000).offset_pct(-0.5), Some(p(99_500)));
        assert_eq!(p(100_000).offset_pct(1.0), Some(p(101_000)));
    }
}
