//! Async Rust client for the [DhanHQ v2 API](https://dhanhq.co/docs/v2/).
//!
//! The crate is a self-contained broker SDK: it knows nothing about strategies
//! or the rest of the trading system, so it can be reused anywhere.
//!
//! | Area | Entry point |
//! |---|---|
//! | REST (orders, super orders, portfolio, funds, quotes, history, ...) | [`DhanClient`] |
//! | Access-token generation (TOTP, API key consent flow) | [`auth::DhanAuth`] |
//! | Live market feed (binary WebSocket) | [`feed::MarketFeed`] |
//! | Live order updates (JSON WebSocket) | [`order_update::OrderUpdateStream`] |
//! | Instrument master (security IDs, tick sizes, lot sizes) | [`instruments::InstrumentMaster`] |
//!
//! # Safety properties
//!
//! * Every call goes through a client-side rate limiter sized to Dhan's
//!   published limits, so the account is never throttled or blocked.
//! * Read-only calls are retried on transient failures. Order placement,
//!   modification and cancellation are **never** retried automatically: a
//!   duplicate order is worse than a failed one. When the outcome of such a
//!   call is unknown (for example a timeout after the request was sent),
//!   [`DhanError::outcome_unknown`] returns `true` and the caller should look
//!   the order up by its correlation ID.
//! * Prices are [`rust_decimal::Decimal`], never `f64`.
//! * Access tokens are wrapped in [`Secret`], which never prints its value.
//! * The WebSocket clients reconnect with backoff and resubscribe on their own.

// The no-panic lints guard production code; tests may index and unwrap.
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
mod rate_limit;
mod serde_util;
mod types;
mod ws;

pub mod auth;
pub mod feed;
pub mod instruments;
pub mod order_update;
pub mod rest;

pub use config::{DhanConfig, Secret};
pub use error::{DhanError, ErrorKind};
pub use rate_limit::{ApiCategory, Limit, RateLimitConfig};
pub use rest::*;
pub use types::*;
pub use ws::ReconnectPolicy;

/// Convenience alias used throughout the crate.
pub type Result<T, E = DhanError> = std::result::Result<T, E>;

pub use rust_decimal::Decimal;
