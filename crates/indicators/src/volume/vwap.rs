use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Session VWAP: `vwap`. Volume-weighted typical price
/// (`(high + low + close) / 3`) since the first bar of the day; resets every
/// session. `None` until some volume has traded.
#[derive(Debug, Clone, Default)]
pub struct Vwap {
    price_volume: f64,
    volume: f64,
}

impl Vwap {
    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(0)?;
        Ok(Box::new(Self::default()))
    }
}

impl Indicator for Vwap {
    fn update(&mut self, bar: &Bar, ctx: &BarContext) {
        if ctx.new_session {
            *self = Self::default();
        }
        let v = bar.volume as f64;
        self.price_volume += bar.typical_price() * v;
        self.volume += v;
    }

    fn value(&self, _: usize) -> Option<f64> {
        (self.volume > 0.0).then(|| self.price_volume / self.volume)
    }
}
