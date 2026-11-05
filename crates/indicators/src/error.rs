use trading_core::{InstrumentId, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndicatorError {
    #[error("cannot parse indicator spec {spec:?}: {why}")]
    Parse { spec: String, why: &'static str },

    #[error("unknown indicator {0:?}")]
    Unknown(String),

    #[error("{spec}: {why}")]
    BadParams { spec: String, why: String },

    #[error("{spec}: no output {output:?} (has {available})")]
    UnknownOutput {
        spec: String,
        output: String,
        available: String,
    },

    #[error("too many indicators")]
    TooMany,

    #[error("bar for unknown instrument {0}")]
    UnknownInstrument(InstrumentId),

    #[error("bar for {instrument} at {ts} is not after the previous one at {last}; ignored")]
    OutOfOrder {
        instrument: InstrumentId,
        ts: Timestamp,
        last: Timestamp,
    },
}
