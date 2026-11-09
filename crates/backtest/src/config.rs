use chrono::NaiveDate;
use costs::CostConfig;
use risk::RiskConfig;
use serde::Deserialize;

/// A backtest, as written in a config file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestConfig {
    pub data: DataConfig,
    #[serde(default)]
    pub broker: BrokerConfig,
    #[serde(default)]
    pub risk: RiskConfig,
    #[serde(default)]
    pub costs: CostConfig,
    pub strategy: Vec<StrategyConfig>,
}

/// Where the bars come from.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase", deny_unknown_fields)]
pub enum DataConfig {
    /// Random data, to test the machinery. Says nothing about profit.
    Synthetic {
        from: NaiveDate,
        to: NaiveDate,
        stocks: u32,
        #[serde(default = "default_seed")]
        seed: u64,
    },
    /// 1-minute bars in QuestDB, loaded by `dhan-history`.
    Questdb {
        from: NaiveDate,
        to: NaiveDate,
        /// CSV written by `dhan-history`.
        universe_file: String,
        #[serde(default = "default_addr")]
        addr: String,
    },
}

fn default_seed() -> u64 {
    42
}

fn default_addr() -> String {
    "127.0.0.1:9000".into()
}

/// How the simulated broker behaves.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrokerConfig {
    pub seed: u64,
    pub max_participation: f64,
    pub slippage_multiplier: f64,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            seed: 1,
            max_participation: 0.10,
            slippage_multiplier: 1.0,
        }
    }
}

/// One strategy in the run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyConfig {
    /// Name it is registered under in `strategies::build`.
    pub name: String,
    /// Symbols it trades; every stock if left out.
    pub stocks: Option<Vec<String>>,
    /// The strategy's own settings; anything left out takes its default.
    #[serde(default = "empty_params")]
    pub params: serde_json::Value,
}

fn empty_params() -> serde_json::Value {
    serde_json::json!({})
}
