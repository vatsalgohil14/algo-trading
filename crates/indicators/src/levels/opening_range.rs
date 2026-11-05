use trading_core::{Bar, Price};

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Opening range: `orb(minutes)`, the high and low of the first `minutes`
/// one-minute bars of the session. Outputs `high`, `low`, `range`.
///
/// Strict on purpose: if any bar of the window is missing (e.g. the feed was
/// down at the open), the range stays unavailable for the whole day, rather
/// than producing a level a breakout strategy would trade off wrongly.
#[derive(Debug, Clone)]
pub struct OpeningRange {
    minutes: u16,
    bars: u16,
    broken: bool,
    high: Price,
    low: Price,
}

impl OpeningRange {
    pub fn new(minutes: u16) -> Self {
        Self {
            minutes: minutes.max(1),
            bars: 0,
            broken: false,
            high: Price::ZERO,
            low: Price::ZERO,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        let minutes = spec.period(0, 375)?;
        Ok(Box::new(Self::new(u16::try_from(minutes).unwrap_or(375))))
    }

    fn complete(&self) -> bool {
        !self.broken && self.bars == self.minutes
    }
}

impl Indicator for OpeningRange {
    fn outputs(&self) -> &'static [&'static str] {
        &["high", "low", "range"]
    }

    fn update(&mut self, bar: &Bar, ctx: &BarContext) {
        if ctx.new_session {
            *self = Self::new(self.minutes);
        }
        let Some(minute) = ctx.session_minute else {
            return;
        };
        if self.broken || minute >= self.minutes {
            // A window that never filled is broken: some bars were missing.
            if minute >= self.minutes && self.bars < self.minutes {
                self.broken = true;
            }
            return;
        }
        if minute != self.bars {
            self.broken = true; // a bar inside the window is missing
            return;
        }
        if self.bars == 0 {
            (self.high, self.low) = (bar.high, bar.low);
        } else {
            self.high = self.high.max(bar.high);
            self.low = self.low.min(bar.low);
        }
        self.bars += 1;
    }

    fn value(&self, index: usize) -> Option<f64> {
        if !self.complete() {
            return None;
        }
        match index {
            0 => Some(self.high.as_f64()),
            1 => Some(self.low.as_f64()),
            2 => Some((self.high - self.low).as_f64()),
            _ => None,
        }
    }
}
