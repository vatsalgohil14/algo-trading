//! Simulation for testing the system without a broker.
//!
//! * [`synthetic`] — reproducible, realistic-looking 1-minute bars, for
//!   exercising the pipeline end to end. They say nothing about whether a
//!   strategy makes money: there is no real edge to find in random data.
//! * [`broker`] — a pessimistic simulated broker with injectable failures.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod broker;
mod rng;
pub mod synthetic;
