use trading_core::{Bar, Price};

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Average True Range of whole trading days: `atr_daily(days)`, in rupees,
/// built from 1-minute bars.
///
/// Uses completed days only, so the value is fixed for the whole of today
/// and never peeks at today's bars. Wilder smoothing, seeded with the simple
/// average of the first `days` daily true ranges (the very first day has no
/// previous close and is used only for that close). Available once
/// `days + 1` sessions have finished.
#[derive(Debug, Clone)]
pub struct DailyAtr {
    days: usize,
    /// High, low and close of the session in progress.
    today: Option<(Price, Price, Price)>,
    prev_close: Option<Price>,
    seen: usize,
    value: f64,
}

impl DailyAtr {
    pub fn new(days: usize) -> Self {
        Self {
            days: days.max(1),
            today: None,
            prev_close: None,
            seen: 0,
            value: 0.0,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 1_000)?)))
    }

    fn close_day(&mut self, (high, low, close): (Price, Price, Price)) {
        if let Some(pc) = self.prev_close {
            let (h, l, pc) = (high.as_f64(), low.as_f64(), pc.as_f64());
            let tr = (h - l).max((h - pc).abs()).max((l - pc).abs());
            let n = self.days as f64;
            if self.seen < self.days {
                self.value += tr / n;
            } else {
                self.value = (self.value * (n - 1.0) + tr) / n;
            }
            self.seen = self.seen.saturating_add(1);
        }
        self.prev_close = Some(close);
    }
}

impl Indicator for DailyAtr {
    fn update(&mut self, bar: &Bar, ctx: &BarContext) {
        if ctx.new_session
            && let Some(day) = self.today.take()
        {
            self.close_day(day);
        }
        self.today = Some(match self.today {
            Some((h, l, _)) => (h.max(bar.high), l.min(bar.low), bar.close),
            None => (bar.high, bar.low, bar.close),
        });
    }

    fn value(&self, _: usize) -> Option<f64> {
        (self.seen >= self.days).then_some(self.value)
    }
}
