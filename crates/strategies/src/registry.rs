//! Builds strategies by name, so a config file can choose them.

use indicators::BankBuilder;
use serde::de::DeserializeOwned;
use trading_core::StrategyId;

use crate::orb::{Orb, OrbConfig};
use crate::template::{Template, TemplateConfig};
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

/// Builds the strategy called `name` with settings `value` (an object;
/// fields left out take their defaults).
///
/// To add a strategy, add one line here.
pub fn build(
    name: &str,
    id: StrategyId,
    value: serde_json::Value,
    bank: &mut BankBuilder,
    instruments: usize,
) -> Result<Box<dyn Strategy>, StrategyError> {
    match name {
        "orb" => Ok(Box::new(Orb::new(
            id,
            params::<OrbConfig>("orb", value)?,
            bank,
            instruments,
        )?)),
        "template" => Ok(Box::new(Template::new(
            id,
            params::<TemplateConfig>("template", value)?,
            bank,
            instruments,
        )?)),
        other => Err(StrategyError::UnknownStrategy(other.to_owned())),
    }
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
    }
}
