//! Storage for the trading system on QuestDB, using QWP (the QuestDB Wire
//! Protocol: binary, columnar, compressed, over WebSocket on port 9000).
//!
//! * [`Store`] — background writer. Trading code hands it rows and moves on;
//!   a dedicated thread batches them and sends them over QWP. With a
//!   store-and-forward directory configured, rows survive a QuestDB outage
//!   and a crash of this process, and are replayed on reconnect.
//! * [`rows`] — one typed struct per table in `deploy/questdb/schema.sql`.
//! * [`query`] — reads over QWP, e.g. loading bars for a backtest.
//!
//! The schema is created by `scripts/questdb.sh init`; this crate only writes
//! into existing tables.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod config;
mod error;
pub mod query;
pub mod rows;
pub mod schema;
mod writer;

pub use config::{Durability, StoreConfig};
pub use error::StoreError;
pub use rows::{Mode, Record};
pub use writer::{Store, StoreStats};

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
