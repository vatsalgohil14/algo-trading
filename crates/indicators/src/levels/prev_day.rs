use trading_core::{Bar, Price};

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Previous session's high, low and close: `prev_day`. Outputs `high`,
/// `low`, `close`. Available from the second day of data.
#[derive(Debug, Clone, Default)]
pub struct PrevDay {
    today: Option<(Price, Price, Price)>,
    previous: Option<(Price, Price, Price)>,
}

impl PrevDay {
    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(0)?;
        Ok(Box::new(Self::default()))
    }
}

impl Indicator for PrevDay {
    fn outputs(&self) -> &'static [&'static str] {
        &["high", "low", "close"]
    }

    fn update(&mut self, bar: &Bar, ctx: &BarContext) {
        if ctx.new_session && self.today.is_some() {
            self.previous = self.today.take();
        }
        self.today = Some(match self.today {
            Some((h, l, _)) => (h.max(bar.high), l.min(bar.low), bar.close),
            None => (bar.high, bar.low, bar.close),
        });
    }

    fn value(&self, index: usize) -> Option<f64> {
        let (h, l, c) = self.previous?;
        match index {
            0 => Some(h.as_f64()),
            1 => Some(l.as_f64()),
            2 => Some(c.as_f64()),
            _ => None,
        }
    }
}
