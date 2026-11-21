//! Ranking strategies: each scores a stock on a day; the rotation holds the
//! highest scores. Settings are `serde` structs with defaults, and bad
//! values are rejected when the strategy is built.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RotationError;
use crate::panel::{MONTH, Panel};

/// Scores stocks for a rotation. Higher is better; `None` when the stock
/// lacks the history the score needs.
pub trait Ranker: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn score(&self, p: &Panel, stock: usize, day: usize) -> Option<f64>;
}

/// Plain price momentum: the return over `lookback_months`, skipping the
/// most recent `skip_months`. The classic is 12-1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MomentumConfig {
    pub lookback_months: usize,
    pub skip_months: usize,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            lookback_months: 12,
            skip_months: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Momentum {
    name: String,
    lookback: usize,
    skip: usize,
}

impl Momentum {
    pub fn new(c: MomentumConfig) -> Result<Self, RotationError> {
        if c.lookback_months == 0 || c.lookback_months > 36 || c.skip_months >= c.lookback_months {
            return Err(RotationError::Config(format!(
                "momentum: need 0 < skip_months < lookback_months <= 36, got {}-{}",
                c.lookback_months, c.skip_months
            )));
        }
        Ok(Self {
            name: format!("momentum_{}_{}", c.lookback_months, c.skip_months),
            lookback: c.lookback_months * MONTH,
            skip: c.skip_months * MONTH,
        })
    }
}

impl Ranker for Momentum {
    fn name(&self) -> &str {
        &self.name
    }

    fn score(&self, p: &Panel, s: usize, d: usize) -> Option<f64> {
        p.momentum(s, d, self.lookback, self.skip)
    }
}

/// Momentum that prefers smooth risers: the average of 12-1 and 6-1
/// momentum, each divided by 1-year volatility, minus `vol_penalty` × that
/// volatility. It earned slightly less than plain momentum in testing, with
/// about half the drawdown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MomentumLowVolConfig {
    pub vol_penalty: f64,
}

impl Default for MomentumLowVolConfig {
    fn default() -> Self {
        Self { vol_penalty: 3.0 }
    }
}

#[derive(Debug, Clone)]
pub struct MomentumLowVol {
    vol_penalty: f64,
}

impl MomentumLowVol {
    pub fn new(c: MomentumLowVolConfig) -> Result<Self, RotationError> {
        if !c.vol_penalty.is_finite() || !(0.0..=20.0).contains(&c.vol_penalty) {
            return Err(RotationError::Config(format!(
                "momentum_low_vol: vol_penalty must be 0..20, got {}",
                c.vol_penalty
            )));
        }
        Ok(Self {
            vol_penalty: c.vol_penalty,
        })
    }
}

impl Ranker for MomentumLowVol {
    fn name(&self) -> &str {
        "momentum_low_vol"
    }

    fn score(&self, p: &Panel, s: usize, d: usize) -> Option<f64> {
        let v = p.volatility(s, d, 12 * MONTH)?;
        (v > 0.0).then_some(())?;
        let m12 = p.momentum(s, d, 12 * MONTH, MONTH)?;
        let m6 = p.momentum(s, d, 6 * MONTH, MONTH)?;
        Some((m12 / v + m6 / v) / 2.0 - self.vol_penalty * v)
    }
}

/// A ranking from a plain function, for research.
pub struct FnRanker {
    pub name: &'static str,
    pub score: fn(&Panel, usize, usize) -> Option<f64>,
}

impl std::fmt::Debug for FnRanker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

impl Ranker for FnRanker {
    fn name(&self) -> &str {
        self.name
    }

    fn score(&self, p: &Panel, s: usize, d: usize) -> Option<f64> {
        (self.score)(p, s, d)
    }
}

/// Strategies that can be built by name.
pub const STRATEGIES: [&str; 2] = ["momentum", "momentum_low_vol"];

/// Builds a strategy by name with JSON settings, as a config file would.
pub fn build(name: &str, params: Value) -> Result<Box<dyn Ranker>, RotationError> {
    let params = if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    };
    let bad = |e: serde_json::Error| RotationError::Config(format!("{name}: {e}"));
    Ok(match name {
        "momentum" => Box::new(Momentum::new(serde_json::from_value(params).map_err(bad)?)?),
        "momentum_low_vol" => Box::new(MomentumLowVol::new(
            serde_json::from_value(params).map_err(bad)?,
        )?),
        other => {
            return Err(RotationError::Config(format!(
                "unknown strategy {other:?}; known: {}",
                STRATEGIES.join(", ")
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use serde_json::json;

    use super::*;
    use crate::panel::tests::panel;

    #[test]
    fn builds_by_name_and_rejects_bad_settings() {
        assert_eq!(
            build("momentum", Value::Null).unwrap().name(),
            "momentum_12_1"
        );
        let m = build("momentum", json!({"lookback_months": 12, "skip_months": 2})).unwrap();
        assert_eq!(m.name(), "momentum_12_2");
        assert!(build("momentum", json!({"lookback_months": 3, "skip_months": 3})).is_err());
        assert!(build("momentum", json!({"typo": 1})).is_err());
        assert!(build("momentum_low_vol", json!({"vol_penalty": -1})).is_err());
        assert!(build("nope", Value::Null).is_err());
    }

    #[test]
    fn smooth_riser_beats_jumpy_riser_with_the_same_gain() {
        // Both end 1.5x up over 300 days; B zig-zags.
        let a: Vec<Option<f64>> = (0..300)
            .map(|i| Some(100.0 * 1.5f64.powf(f64::from(i) / 299.0)))
            .collect();
        let b: Vec<Option<f64>> = a
            .iter()
            .enumerate()
            .map(|(i, x)| x.map(|v| if i % 2 == 0 { v * 1.04 } else { v * 0.96 }))
            .collect();
        let p = panel(&[&a, &b]);
        let r = MomentumLowVol::new(MomentumLowVolConfig::default()).unwrap();
        assert!(r.score(&p, 0, 299).unwrap() > r.score(&p, 1, 299).unwrap());
        let m = Momentum::new(MomentumConfig::default()).unwrap();
        assert!(m.score(&p, 0, 299).unwrap() > 0.0);
        assert!(m.score(&p, 0, 200).is_none()); // not enough history
    }
}
