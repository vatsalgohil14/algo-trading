//! Builds strategies by name, so a config file can choose them.

use std::collections::BTreeMap;

use indicators::BankBuilder;
use serde::de::DeserializeOwned;
use trading_core::StrategyId;

use crate::orb::Orb;
use crate::template::Template;
use crate::{Strategy, StrategyError};

fn params<T: DeserializeOwned>(
    name: &'static str,
    value: serde_json::Value,
) -> Result<T, StrategyError> {
    serde_json::from_value(value).map_err(|e| StrategyError::Config {
        strategy: name,
        why: e.to_string(),
    })
}

/// Makes a strategy from its settings. See [`Registry::add`].
pub type Factory = Box<
    dyn Fn(
            StrategyId,
            serde_json::Value,
            &mut BankBuilder,
            usize,
        ) -> Result<Box<dyn Strategy>, StrategyError>
        + Send
        + Sync,
>;

/// Strategies by name, so a config file can choose them.
pub struct Registry {
    factories: BTreeMap<&'static str, Factory>,
    /// Strategies kept for tests and the record, with why they were
    /// retired. Real runs refuse them; see [`Registry::retired`].
    retired: BTreeMap<&'static str, &'static str>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.factories.keys()).finish()
    }
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            factories: BTreeMap::new(),
            retired: BTreeMap::new(),
        }
    }

    /// Every strategy in this crate.
    ///
    /// To add a strategy, add one line here.
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.add("orb", Orb::new);
        r.retire(
            "orb",
            "failed its first real backtest (Nifty 50, 2022-2026): profit factor 0.52, \
             losing even before costs at every nearby setting; see docs/strategies/orb.md",
        );
        r.add("template", Template::new);
        r
    }

    /// Marks `name` as retired with the reason. It can still be built (tests
    /// use it as an example), but tools that trade or backtest for real
    /// should refuse it.
    pub fn retire(&mut self, name: &'static str, why: &'static str) {
        self.retired.insert(name, why);
    }

    /// Why `name` was retired, or `None` if it is in use.
    pub fn retired(&self, name: &str) -> Option<&'static str> {
        self.retired.get(name).copied()
    }

    /// Registers `new` under `name`. Its settings type `C` is read from the
    /// config's `[strategy.params]`; unknown fields are errors if `C` says
    /// `deny_unknown_fields`, and missing ones take its defaults. Adding a
    /// name twice replaces the first.
    pub fn add<C, S>(
        &mut self,
        name: &'static str,
        new: fn(StrategyId, C, &mut BankBuilder, usize) -> Result<S, StrategyError>,
    ) where
        C: DeserializeOwned + 'static,
        S: Strategy + 'static,
    {
        let factory: Factory = Box::new(move |id, value, bank, instruments| {
            let strategy = new(id, params::<C>(name, value)?, bank, instruments)?;
            Ok(Box::new(strategy))
        });
        self.factories.insert(name, factory);
    }

    /// Builds the strategy called `name` with settings `value` (an object;
    /// fields left out take their defaults).
    pub fn build(
        &self,
        name: &str,
        id: StrategyId,
        value: serde_json::Value,
        bank: &mut BankBuilder,
        instruments: usize,
    ) -> Result<Box<dyn Strategy>, StrategyError> {
        let factory = self
            .factories
            .get(name)
            .ok_or_else(|| StrategyError::UnknownStrategy(name.to_owned()))?;
        factory(id, value, bank, instruments)
    }

    /// Registered names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.factories.keys().copied()
    }
}

/// Builds a strategy from [`Registry::builtin`].
pub fn build(
    name: &str,
    id: StrategyId,
    value: serde_json::Value,
    bank: &mut BankBuilder,
    instruments: usize,
) -> Result<Box<dyn Strategy>, StrategyError> {
    Registry::builtin().build(name, id, value, bank, instruments)
}

#[cfg(test)]
mod tests {
    use indicators::Registry;

    use super::*;

    #[test]
    fn builds_by_name_and_checks_settings() {
        let mut b = BankBuilder::new(Registry::builtin());
        let id = StrategyId(0);
        let orb = build("orb", id, serde_json::json!({"target_r": 3.0}), &mut b, 2).unwrap();
        assert_eq!(orb.name(), "orb");
        assert!(matches!(
            build("nope", id, serde_json::json!({}), &mut b, 2),
            Err(StrategyError::UnknownStrategy(_))
        ));
        assert!(build("orb", id, serde_json::json!({"typo": 1}), &mut b, 2).is_err());
        assert!(build("orb", id, serde_json::json!({"target_r": -1}), &mut b, 2).is_err());
        let r = super::Registry::builtin();
        assert_eq!(r.names().collect::<Vec<_>>(), ["orb", "template"]);
        assert!(r.retired("orb").is_some());
        assert!(r.retired("template").is_none());
    }
}
