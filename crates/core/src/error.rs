/// Something failed validation. Raised at the edges (loading instruments,
/// building orders, accepting bars) so that bad values never reach the
/// trading logic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("invalid instrument: {0}")]
    InvalidInstrument(String),

    #[error("duplicate instrument: {0}")]
    DuplicateInstrument(String),

    #[error("invalid bar: {0}")]
    InvalidBar(String),

    #[error("invalid order: {0}")]
    InvalidOrder(String),

    #[error("invalid signal: {0}")]
    InvalidSignal(String),

    #[error("invalid client order id {0:?}: must be 1-{max} chars of [A-Za-z0-9_-]", max = crate::ClientOrderId::MAX_LEN)]
    InvalidClientOrderId(String),

    #[error("clock cannot go backwards: at {now}, asked for {requested}")]
    ClockBackwards { now: i64, requested: i64 },
}
