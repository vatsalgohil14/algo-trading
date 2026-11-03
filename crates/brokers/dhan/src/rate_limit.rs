//! Client-side rate limiting matching Dhan's published limits.
//!
//! Each API category has several windows (per second, minute, hour, day).
//! A call takes one slot from every window of its category. If the wait for
//! a slot would exceed `max_wait`, the call fails fast with
//! [`DhanError::RateLimitedLocally`] instead of blocking a trading decision.
//!
//! Limits are tracked per process. Restarting resets the counters, while
//! Dhan's own counters do not reset.

use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

use crate::{DhanError, Result};

/// Dhan's rate-limit buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiCategory {
    /// Place / modify / cancel orders, super orders.
    Order,
    /// Historical data.
    Data,
    /// Market quote snapshots (`/marketfeed/*`).
    Quote,
    /// Everything else: order book, positions, funds, profile, ...
    NonTrading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub count: u32,
    pub per: Duration,
}

impl Limit {
    pub const fn per_second(count: u32) -> Self {
        Self {
            count,
            per: Duration::from_secs(1),
        }
    }
    pub const fn per_minute(count: u32) -> Self {
        Self {
            count,
            per: Duration::from_secs(60),
        }
    }
    pub const fn per_hour(count: u32) -> Self {
        Self {
            count,
            per: Duration::from_secs(3600),
        }
    }
    pub const fn per_day(count: u32) -> Self {
        Self {
            count,
            per: Duration::from_secs(86_400),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub order: Vec<Limit>,
    pub data: Vec<Limit>,
    pub quote: Vec<Limit>,
    pub non_trading: Vec<Limit>,
    /// Longest a call may wait for a slot before failing.
    pub max_wait: Duration,
}

impl Default for RateLimitConfig {
    /// Dhan's published limits (docs: Introduction → Rate Limit).
    fn default() -> Self {
        Self {
            order: vec![
                Limit::per_second(10),
                Limit::per_minute(250),
                Limit::per_hour(1000),
                Limit::per_day(7000),
            ],
            data: vec![Limit::per_second(5), Limit::per_day(100_000)],
            quote: vec![Limit::per_second(1)],
            non_trading: vec![Limit::per_second(20)],
            max_wait: Duration::from_secs(5),
        }
    }
}

pub(crate) struct RateLimiters {
    order: Vec<DefaultDirectRateLimiter>,
    data: Vec<DefaultDirectRateLimiter>,
    quote: Vec<DefaultDirectRateLimiter>,
    non_trading: Vec<DefaultDirectRateLimiter>,
    max_wait: Duration,
    clock: DefaultClock,
}

impl std::fmt::Debug for RateLimiters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiters")
            .field("max_wait", &self.max_wait)
            .finish_non_exhaustive()
    }
}

fn build(limits: &[Limit]) -> Result<Vec<DefaultDirectRateLimiter>> {
    limits
        .iter()
        .map(|l| {
            let burst = NonZeroU32::new(l.count)
                .ok_or_else(|| DhanError::InvalidRequest("rate limit count must be > 0".into()))?;
            let quota = Quota::with_period(l.per / l.count)
                .ok_or_else(|| DhanError::InvalidRequest("rate limit period must be > 0".into()))?
                .allow_burst(burst);
            Ok(RateLimiter::direct(quota))
        })
        .collect()
}

impl RateLimiters {
    pub(crate) fn new(cfg: &RateLimitConfig) -> Result<Self> {
        Ok(Self {
            order: build(&cfg.order)?,
            data: build(&cfg.data)?,
            quote: build(&cfg.quote)?,
            non_trading: build(&cfg.non_trading)?,
            max_wait: cfg.max_wait,
            clock: DefaultClock::default(),
        })
    }

    /// Waits for a slot in every window of `category`.
    pub(crate) async fn acquire(&self, category: ApiCategory) -> Result<()> {
        let limiters = match category {
            ApiCategory::Order => &self.order,
            ApiCategory::Data => &self.data,
            ApiCategory::Quote => &self.quote,
            ApiCategory::NonTrading => &self.non_trading,
        };
        for limiter in limiters {
            loop {
                match limiter.check() {
                    Ok(()) => break,
                    Err(not_until) => {
                        let wait = not_until.wait_time_from(self.clock.now());
                        if wait > self.max_wait {
                            return Err(DhanError::RateLimitedLocally { category, wait });
                        }
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fails_fast_when_wait_is_too_long() -> Result<()> {
        let cfg = RateLimitConfig {
            order: vec![Limit::per_hour(2)],
            max_wait: Duration::from_millis(50),
            ..RateLimitConfig::default()
        };
        let limits = RateLimiters::new(&cfg)?;
        limits.acquire(ApiCategory::Order).await?;
        limits.acquire(ApiCategory::Order).await?;
        let third = limits.acquire(ApiCategory::Order).await;
        assert!(matches!(third, Err(DhanError::RateLimitedLocally { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn waits_briefly_when_allowed() -> Result<()> {
        let cfg = RateLimitConfig {
            quote: vec![Limit {
                count: 1,
                per: Duration::from_millis(20),
            }],
            ..RateLimitConfig::default()
        };
        let limits = RateLimiters::new(&cfg)?;
        let start = std::time::Instant::now();
        limits.acquire(ApiCategory::Quote).await?;
        limits.acquire(ApiCategory::Quote).await?;
        assert!(start.elapsed() >= Duration::from_millis(15));
        Ok(())
    }
}
