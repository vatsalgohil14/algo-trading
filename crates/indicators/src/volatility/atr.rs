use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Average True Range with Wilder smoothing: `atr(period)`, in rupees.
///
/// True range includes the jump from the previous close, so the overnight
/// gap counts on the first bar of a day. Seeded with the simple average of
/// the first `period` true ranges.
#[derive(Debug, Clone)]
pub struct Atr {
    period: usize,
    prev_close: Option<f64>,
    seen: usize,
    value: f64,
}

impl Atr {
    pub fn new(period: usize) -> Self {
        Self {
            period: period.max(1),
            prev_close: None,
            seen: 0,
            value: 0.0,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 10_000)?)))
    }
}

impl Indicator for Atr {
    fn update(&mut self, bar: &Bar, _: &BarContext) {
        let (high, low) = (bar.high.as_f64(), bar.low.as_f64());
        let tr = match self.prev_close {
            Some(pc) => (high - low).max((high - pc).abs()).max((low - pc).abs()),
            None => high - low,
        };
        let n = self.period as f64;
        if self.seen < self.period {
            self.value += tr / n;
        } else {
            self.value = (self.value * (n - 1.0) + tr) / n;
        }
        self.seen = self.seen.saturating_add(1);
        self.prev_close = Some(bar.close.as_f64());
    }

    fn value(&self, _: usize) -> Option<f64> {
        (self.seen >= self.period).then_some(self.value)
    }
}
