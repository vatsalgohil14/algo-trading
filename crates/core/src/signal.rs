//! What a strategy asks for. Strategies never size or place orders: they
//! emit signals, and the risk manager decides whether and how much.

use crate::{CoreError, InstrumentId, Price, StrategyId, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    EnterLong,
    EnterShort,
    /// Close this strategy's position in the instrument and cancel its
    /// working orders there (including an entry that has not filled).
    Exit,
    /// Move the protective stop of the open position to `stop`, e.g. to
    /// breakeven. Only ever tightens risk; the risk manager rejects a move
    /// that widens it.
    AdjustStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub ts: Timestamp,
    pub intent: Intent,
    /// Limit price for the entry; `None` means enter at market.
    pub entry: Option<Price>,
    /// Protective stop. Required for entries: it sets the position size
    /// (risk per trade / distance to stop) and is placed at the broker.
    pub stop: Option<Price>,
    pub target: Option<Price>,
    /// Short machine-readable reason, e.g. `"orb_breakout"`, stored with the
    /// signal for later analysis.
    pub reason: &'static str,
}

impl Signal {
    /// Checks the signal is self-consistent. `reference` is the price the
    /// entry is expected at when `entry` is `None` (usually the last close).
    pub fn validate(&self, reference: Price) -> Result<(), CoreError> {
        let fail = |why: &str| {
            Err(CoreError::InvalidSignal(format!(
                "strategy {} on {} ({}): {why}",
                self.strategy.0, self.instrument, self.reason
            )))
        };
        let long = match self.intent {
            Intent::Exit => return Ok(()),
            Intent::AdjustStop => {
                return match self.stop {
                    Some(stop) if stop.is_positive() => Ok(()),
                    _ => fail("a stop adjustment needs a positive stop"),
                };
            }
            Intent::EnterLong => true,
            Intent::EnterShort => false,
        };
        let entry = self.entry.unwrap_or(reference);
        if !entry.is_positive() {
            return fail("entry price must be positive");
        }
        let Some(stop) = self.stop else {
            return fail("entries need a stop");
        };
        if !stop.is_positive() || (long && stop >= entry) || (!long && stop <= entry) {
            return fail("stop must be on the losing side of the entry");
        }
        if let Some(target) = self.target
            && ((long && target <= entry) || (!long && target >= entry))
        {
            return fail("target must be on the winning side of the entry");
        }
        Ok(())
    }

    /// Distance from entry to stop, the risk per share.
    pub fn risk_per_share(&self, reference: Price) -> Option<Price> {
        let entry = self.entry.unwrap_or(reference);
        let stop = self.stop?;
        let d = (entry - stop).paise().abs();
        (d > 0).then_some(Price::from_paise(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long(stop: i64, target: Option<i64>) -> Signal {
        Signal {
            strategy: StrategyId(1),
            instrument: InstrumentId(2),
            ts: Timestamp::from_secs(0),
            intent: Intent::EnterLong,
            entry: Some(Price::from_paise(10_000)),
            stop: Some(Price::from_paise(stop)),
            target: target.map(Price::from_paise),
            reason: "test",
        }
    }

    #[test]
    fn validation() {
        long(9_900, Some(10_300)).validate(Price::ZERO).unwrap();
        assert!(long(10_000, None).validate(Price::ZERO).is_err());
        assert!(long(9_900, Some(9_950)).validate(Price::ZERO).is_err());
        let no_stop = Signal {
            stop: None,
            ..long(9_900, None)
        };
        assert!(no_stop.validate(Price::ZERO).is_err());
        let short = Signal {
            intent: Intent::EnterShort,
            ..long(10_100, Some(9_700))
        };
        short.validate(Price::ZERO).unwrap();
        // Market entry falls back to the reference price.
        let market = Signal {
            entry: None,
            ..long(9_900, None)
        };
        assert!(market.validate(Price::ZERO).is_err());
        market.validate(Price::from_paise(10_000)).unwrap();
        let exit = Signal {
            intent: Intent::Exit,
            stop: None,
            ..long(0, None)
        };
        exit.validate(Price::ZERO).unwrap();
        let adjust = Signal {
            intent: Intent::AdjustStop,
            ..long(10_000, None)
        };
        adjust.validate(Price::ZERO).unwrap();
        assert!(
            Signal {
                stop: None,
                ..adjust
            }
            .validate(Price::ZERO)
            .is_err()
        );
    }

    #[test]
    fn risk_per_share() {
        assert_eq!(
            long(9_900, None).risk_per_share(Price::ZERO),
            Some(Price::from_paise(100))
        );
        let no_stop = Signal {
            stop: None,
            ..long(9_900, None)
        };
        assert_eq!(no_stop.risk_per_share(Price::ZERO), None);
    }
}
