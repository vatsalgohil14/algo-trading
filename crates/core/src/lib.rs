//! The shared vocabulary of the trading system.
//!
//! Every other crate speaks in these types, so that one strategy runs
//! unchanged in backtest, paper and live trading. Only the implementations
//! of three traits differ between those modes:
//!
//! * [`Clock`] — where "now" comes from (wall clock, or the replayed bar time).
//! * [`DataFeed`] — where market data comes from (a broker feed, or QuestDB).
//! * [`Execution`] — where orders go (a broker, or the simulator).
//!
//! Design rules:
//!
//! * **Exact money.** Prices and amounts are integer paise ([`Price`],
//!   [`Money`]), so arithmetic is exact and backtests are bit-for-bit
//!   reproducible. Convert to `Decimal` only at the edges.
//! * **Dense ids.** Instruments are numbered `0..n` when the universe is
//!   loaded ([`InstrumentId`]), so hot-path state lives in plain `Vec`s.
//! * **No panics.** Bad input is a [`CoreError`], never a crash.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod clock;
mod error;
pub mod execution;
pub mod feed;
pub mod ids;
pub mod instrument;
pub mod market;
pub mod money;
pub mod order;
pub mod position;
pub mod signal;
pub mod time;

pub use clock::{Clock, SimClock, SystemClock};
pub use error::CoreError;
pub use execution::{ExecError, Execution, OrderChange, OrderSnapshot, PositionSnapshot};
pub use feed::{DataFeed, FeedError};
pub use ids::{ClientOrderId, InstrumentId, OrderSeq, StrategyId};
pub use instrument::{Exchange, Instrument, Universe};
pub use market::{Bar, FeedStatus, MarketEvent, Tick};
pub use money::{Money, Price, TickRounding};
pub use order::{
    Bracket, BrokerOrderId, ExecutionReport, Fill, OrderEvent, OrderKind, OrderRequest,
    OrderStatus, Product, Side,
};
pub use position::Position;
pub use signal::{Intent, Signal};
pub use time::{MarketHours, MinuteOfDay, Timestamp};

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
