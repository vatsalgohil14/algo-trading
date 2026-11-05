use std::collections::HashMap;
use std::fmt;

use crate::{Indicator, IndicatorError, Spec, levels, momentum, trend, volatility, volume};

/// Builds a fresh indicator from its spec, checking the parameters.
pub type Factory = fn(&Spec) -> Result<Box<dyn Indicator>, IndicatorError>;

/// Maps indicator names to the code that builds them.
#[derive(Clone, Default)]
pub struct Registry {
    factories: HashMap<String, Factory>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&String> = self.factories.keys().collect();
        names.sort();
        f.debug_struct("Registry")
            .field("indicators", &names)
            .finish()
    }
}

impl Registry {
    pub fn empty() -> Self {
        Self::default()
    }

    /// All indicators that ship with this crate. Add new ones here.
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.register("sma", trend::Sma::from_spec);
        r.register("ema", trend::Ema::from_spec);
        r.register("rsi", momentum::Rsi::from_spec);
        r.register("atr", volatility::Atr::from_spec);
        r.register("vwap", volume::Vwap::from_spec);
        r.register("orb", levels::OpeningRange::from_spec);
        r.register("prev_day", levels::PrevDay::from_spec);
        r
    }

    /// Adds or replaces an indicator. Names are lower-case.
    pub fn register(&mut self, name: &str, factory: Factory) {
        self.factories.insert(name.to_ascii_lowercase(), factory);
    }

    pub fn create(&self, spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        let factory = self
            .factories
            .get(&spec.name)
            .ok_or_else(|| IndicatorError::Unknown(spec.name.clone()))?;
        factory(spec)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.factories.keys().map(String::as_str)
    }
}
