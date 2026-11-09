use indicators::IndicatorError;
use trading_core::{InstrumentId, StrategyId};

#[derive(Debug, thiserror::Error)]
pub enum StrategyError {
    #[error("invalid config for {strategy}: {why}")]
    Config { strategy: &'static str, why: String },

    #[error("no strategy is called {0:?}")]
    UnknownStrategy(String),

    #[error(transparent)]
    Indicator(#[from] IndicatorError),

    #[error(
        "strategy ids must be 0, 1, 2, ... in the order added: expected {expected:?}, got {got:?}"
    )]
    StrategyId {
        expected: StrategyId,
        got: StrategyId,
    },

    #[error("instrument {0} is outside the universe")]
    UnknownInstrument(InstrumentId),
}
