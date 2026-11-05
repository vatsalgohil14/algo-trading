use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Relative Strength Index with Wilder smoothing: `rsi(period)`, `0..=100`.
/// Ready after `period` close-to-close changes (`period + 1` bars).
#[derive(Debug, Clone)]
pub struct Rsi {
    period: usize,
    prev_close: Option<f64>,
    changes: usize,
    avg_gain: f64,
    avg_loss: f64,
}

impl Rsi {
    pub fn new(period: usize) -> Self {
        Self {
            period: period.max(1),
            prev_close: None,
            changes: 0,
            avg_gain: 0.0,
            avg_loss: 0.0,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 10_000)?)))
    }
}

impl Indicator for Rsi {
    fn update(&mut self, bar: &Bar, _: &BarContext) {
        let close = bar.close.as_f64();
        if let Some(prev) = self.prev_close {
            let change = close - prev;
            let (gain, loss) = (change.max(0.0), (-change).max(0.0));
            let n = self.period as f64;
            if self.changes < self.period {
                // Simple average over the first `period` changes.
                self.avg_gain += gain / n;
                self.avg_loss += loss / n;
            } else {
                self.avg_gain = (self.avg_gain * (n - 1.0) + gain) / n;
                self.avg_loss = (self.avg_loss * (n - 1.0) + loss) / n;
            }
            self.changes = self.changes.saturating_add(1);
        }
        self.prev_close = Some(close);
    }

    fn value(&self, _: usize) -> Option<f64> {
        if self.changes < self.period {
            return None;
        }
        Some(match (self.avg_gain > 0.0, self.avg_loss > 0.0) {
            (_, true) => 100.0 - 100.0 / (1.0 + self.avg_gain / self.avg_loss),
            (true, false) => 100.0,
            (false, false) => 50.0, // no movement at all
        })
    }
}
