use std::collections::VecDeque;

use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Simple moving average of closes: `sma(period)`.
///
/// The running sum is kept in integer paise, so it never drifts.
#[derive(Debug, Clone)]
pub struct Sma {
    period: usize,
    window: VecDeque<i64>,
    sum: i64,
}

impl Sma {
    pub fn new(period: usize) -> Self {
        Self {
            period: period.max(1),
            window: VecDeque::with_capacity(period.max(1)),
            sum: 0,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 10_000)?)))
    }
}

impl Indicator for Sma {
    fn update(&mut self, bar: &Bar, _: &BarContext) {
        let close = bar.close.paise();
        if self.window.len() == self.period
            && let Some(old) = self.window.pop_front()
        {
            self.sum -= old;
        }
        self.window.push_back(close);
        self.sum += close;
    }

    fn value(&self, _: usize) -> Option<f64> {
        (self.window.len() == self.period).then(|| self.sum as f64 / self.period as f64 / 100.0)
    }
}
