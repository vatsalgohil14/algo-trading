use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Exponential moving average of closes: `ema(period)`, with
/// `alpha = 2 / (period + 1)`. Seeded with the simple average of the first
/// `period` closes, and ready from then on.
#[derive(Debug, Clone)]
pub struct Ema {
    period: usize,
    alpha: f64,
    seen: usize,
    seed_sum: f64,
    value: f64,
}

impl Ema {
    pub fn new(period: usize) -> Self {
        let period = period.max(1);
        Self {
            period,
            alpha: 2.0 / (period as f64 + 1.0),
            seen: 0,
            seed_sum: 0.0,
            value: 0.0,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 10_000)?)))
    }
}

impl Indicator for Ema {
    fn update(&mut self, bar: &Bar, _: &BarContext) {
        let close = bar.close.as_f64();
        if self.seen < self.period {
            self.seen += 1;
            self.seed_sum += close;
            if self.seen == self.period {
                self.value = self.seed_sum / self.period as f64;
            }
        } else {
            self.value += self.alpha * (close - self.value);
        }
    }

    fn value(&self, _: usize) -> Option<f64> {
        (self.seen >= self.period).then_some(self.value)
    }
}
