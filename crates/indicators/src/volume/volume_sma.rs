use std::collections::VecDeque;

use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Average volume per bar: `vol_sma(n)`. Outputs `value`, the average of
/// the last `n` bars including the current one, and `prev`, the average of
/// the `n` bars before the current one.
///
/// `prev` is the one to compare the current bar against ("is this bar's
/// volume 1.5x normal?"), since the bar itself does not dilute it. Runs
/// across days, like the bars themselves. Sums are exact integers.
#[derive(Debug, Clone)]
pub struct VolumeSma {
    period: usize,
    /// The last `period + 1` volumes, oldest first.
    window: VecDeque<u64>,
    sum: u128,
}

impl VolumeSma {
    pub fn new(period: usize) -> Self {
        let period = period.max(1);
        Self {
            period,
            window: VecDeque::with_capacity(period + 1),
            sum: 0,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 10_000)?)))
    }

    fn newest(&self) -> u128 {
        self.window.back().copied().map_or(0, u128::from)
    }

    fn oldest(&self) -> u128 {
        self.window.front().copied().map_or(0, u128::from)
    }
}

impl Indicator for VolumeSma {
    fn outputs(&self) -> &'static [&'static str] {
        &["value", "prev"]
    }

    fn update(&mut self, bar: &Bar, _: &BarContext) {
        if self.window.len() > self.period
            && let Some(old) = self.window.pop_front()
        {
            self.sum -= u128::from(old);
        }
        self.window.push_back(bar.volume);
        self.sum += u128::from(bar.volume);
    }

    fn value(&self, index: usize) -> Option<f64> {
        let n = self.period as f64;
        let full = self.window.len() > self.period;
        match index {
            // The window holds `period + 1` bars once full; drop the oldest.
            0 if full => Some((self.sum - self.oldest()) as f64 / n),
            0 if self.window.len() == self.period => Some(self.sum as f64 / n),
            1 if full => Some((self.sum - self.newest()) as f64 / n),
            _ => None,
        }
    }
}
