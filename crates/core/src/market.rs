//! Market data as the engine sees it, whatever the source.

use crate::{CoreError, InstrumentId, Price, Timestamp};

/// A 1-minute candle. `ts` is the start of the minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub instrument: InstrumentId,
    pub ts: Timestamp,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: u64,
}

impl Bar {
    /// Checks that prices are positive and consistent:
    /// `low <= open, close <= high`. Every bar from outside the process
    /// (broker, database, file) must pass this before the engine sees it.
    pub fn validate(&self) -> Result<(), CoreError> {
        let Bar {
            open,
            high,
            low,
            close,
            ..
        } = *self;
        if !low.is_positive() {
            return Err(self.invalid("non-positive price"));
        }
        if low > open || low > close || high < open || high < close {
            return Err(self.invalid("open/close outside low..high"));
        }
        Ok(())
    }

    fn invalid(&self, why: &str) -> CoreError {
        CoreError::InvalidBar(format!(
            "{} at {}: {why} (o={} h={} l={} c={})",
            self.instrument, self.ts, self.open, self.high, self.low, self.close
        ))
    }

    /// `(high + low + close) / 3` in rupees, the usual VWAP input.
    pub fn typical_price(&self) -> f64 {
        (self.high.as_f64() + self.low.as_f64() + self.close.as_f64()) / 3.0
    }

    pub fn range(&self) -> Price {
        self.high - self.low
    }
}

/// A last-trade update with the best bid and offer, when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub instrument: InstrumentId,
    /// Exchange time of the last trade.
    pub ts: Timestamp,
    pub ltp: Price,
    pub ltq: u32,
    /// Cumulative volume for the day.
    pub volume: u64,
    pub bid: Option<Price>,
    pub ask: Option<Price>,
}

/// Health of the market-data connection. The engine stops opening positions
/// while the feed is not `Up`, because indicators and stops would be acting
/// on stale prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedStatus {
    Up,
    /// Disconnected; the feed is reconnecting.
    Down,
    /// Connected but nothing received for longer than expected.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketEvent {
    Bar(Bar),
    Tick(Tick),
    Status { ts: Timestamp, status: FeedStatus },
}

impl MarketEvent {
    pub fn ts(&self) -> Timestamp {
        match self {
            MarketEvent::Bar(b) => b.ts,
            MarketEvent::Tick(t) => t.ts,
            MarketEvent::Status { ts, .. } => *ts,
        }
    }

    pub fn instrument(&self) -> Option<InstrumentId> {
        match self {
            MarketEvent::Bar(b) => Some(b.instrument),
            MarketEvent::Tick(t) => Some(t.instrument),
            MarketEvent::Status { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: i64, h: i64, l: i64, c: i64) -> Bar {
        Bar {
            instrument: InstrumentId(0),
            ts: Timestamp::from_secs(1_762_400_700),
            open: Price::from_paise(o),
            high: Price::from_paise(h),
            low: Price::from_paise(l),
            close: Price::from_paise(c),
            volume: 1,
        }
    }

    #[test]
    fn validation() {
        assert!(bar(100, 110, 90, 105).validate().is_ok());
        assert!(bar(100, 100, 100, 100).validate().is_ok());
        assert!(bar(100, 99, 90, 95).validate().is_err()); // open above high
        assert!(bar(100, 110, 101, 105).validate().is_err()); // open below low
        assert!(bar(100, 110, 90, 111).validate().is_err()); // close above high
        assert!(bar(0, 0, 0, 0).validate().is_err());
        let err = bar(100, 99, 90, 95).validate().unwrap_err().to_string();
        assert!(err.contains("o=1.00 h=0.99"), "{err}");
    }

    #[test]
    fn derived_values() {
        let b = bar(100, 120, 90, 105);
        assert_eq!(b.range(), Price::from_paise(30));
        assert!((b.typical_price() - 1.05).abs() < 1e-12);
    }
}
