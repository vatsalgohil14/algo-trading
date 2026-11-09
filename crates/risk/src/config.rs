use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::RiskError;

/// Risk limits. Amounts are in rupees; the defaults are for ₹5L of capital.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RiskConfig {
    pub capital: f64,
    /// Exposure per trade. Every trade is at least `min_exposure`; a proven
    /// setup moves towards `max_exposure` according to its score.
    pub min_exposure: f64,
    pub max_exposure: f64,
    /// Most a trade may lose if its stop is hit. The size shrinks to fit;
    /// if that takes it below `min_exposure`, the trade is skipped.
    pub max_loss_per_trade: f64,
    /// Expected reward must be at least this multiple of the expected
    /// round-trip cost (charges and slippage).
    pub min_reward_cost_multiple: f64,
    /// Sum of the exposure of all positions and working entries.
    pub max_gross_exposure: f64,
    /// Longs minus shorts, in either direction.
    pub max_net_exposure: f64,
    /// A loss today (realized, unrealized and charges) of this much stops
    /// all new trades and asks for everything to be closed.
    pub daily_loss_limit: f64,
    /// Intraday leverage for a stock with no value of its own set through
    /// `RiskManager::set_leverage`.
    pub default_leverage: f64,
    /// Score from 0 to 1 per setup (a signal's `reason`). A setup not listed
    /// scores 0, the minimum size. Only add one after out-of-sample results
    /// show it earns more than the others.
    pub setup_scores: BTreeMap<String, f64>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            capital: 500_000.0,
            min_exposure: 30_000.0,
            max_exposure: 50_000.0,
            max_loss_per_trade: 500.0,
            min_reward_cost_multiple: 4.0,
            max_gross_exposure: 1_500_000.0,
            max_net_exposure: 750_000.0,
            daily_loss_limit: 10_000.0,
            default_leverage: 5.0,
            setup_scores: BTreeMap::new(),
        }
    }
}

impl RiskConfig {
    /// A bad config must stop start-up, not misbehave at 09:30.
    pub fn validate(&self) -> Result<(), RiskError> {
        let fail = |why: String| Err(RiskError::Config(why));
        for (name, v) in [
            ("capital", self.capital),
            ("min_exposure", self.min_exposure),
            ("max_exposure", self.max_exposure),
            ("max_loss_per_trade", self.max_loss_per_trade),
            ("max_gross_exposure", self.max_gross_exposure),
            ("max_net_exposure", self.max_net_exposure),
            ("daily_loss_limit", self.daily_loss_limit),
        ] {
            if !v.is_finite() || v <= 0.0 || v > 1e12 {
                return fail(format!("{name} must be a positive amount, got {v}"));
            }
        }
        if !self.min_reward_cost_multiple.is_finite() || self.min_reward_cost_multiple < 0.0 {
            return fail("min_reward_cost_multiple must be a number >= 0".into());
        }
        if self.min_exposure > self.max_exposure {
            return fail("min_exposure must not be above max_exposure".into());
        }
        if self.max_exposure > self.max_gross_exposure {
            return fail("max_exposure must not be above max_gross_exposure".into());
        }
        if self.daily_loss_limit > self.capital {
            return fail("daily_loss_limit must not be above capital".into());
        }
        crate::check_leverage(self.default_leverage)?;
        for (setup, score) in &self.setup_scores {
            if !(0.0..=1.0).contains(score) {
                return fail(format!("score of {setup} must be from 0 to 1, got {score}"));
            }
        }
        Ok(())
    }
}
