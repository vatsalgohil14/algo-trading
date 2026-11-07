use std::collections::VecDeque;

use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Average traded value per day: `turnover(days)`, in rupees, over the last
/// `days` completed sessions (typical price x volume of each bar).
///
/// A liquidity filter: fixed for the whole of today, and never includes
/// today's partial session. Available once `days` sessions have finished.
#[derive(Debug, Clone)]
pub struct DailyTurnover {
    days: usize,
    today: Option<f64>,
    window: VecDeque<f64>,
}

impl DailyTurnover {
    pub fn new(days: usize) -> Self {
        let days = days.max(1);
        Self {
            days,
            today: None,
            window: VecDeque::with_capacity(days),
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 1_000)?)))
    }
}

impl Indicator for DailyTurnover {
    fn update(&mut self, bar: &Bar, ctx: &BarContext) {
        if ctx.new_session
            && let Some(day) = self.today.take()
        {
            if self.window.len() == self.days {
                self.window.pop_front();
            }
            self.window.push_back(day);
        }
        let value = bar.typical_price() * bar.volume as f64;
        *self.today.get_or_insert(0.0) += value;
    }

    fn value(&self, _: usize) -> Option<f64> {
        // Summed afresh (at most `days` numbers) so rounding never drifts.
        (self.window.len() == self.days).then(|| self.window.iter().sum::<f64>() / self.days as f64)
    }
}
