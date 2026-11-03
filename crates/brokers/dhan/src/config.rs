use std::fmt;
use std::time::Duration;

use zeroize::Zeroize;

use crate::rate_limit::RateLimitConfig;
use crate::{DhanError, Result};

/// A string that never shows up in logs or `Debug` output, and is wiped from
/// memory when dropped. Used for access tokens, PINs and API secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the raw value. Call this only at the point where the value is
    /// put on the wire.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Everything needed to talk to Dhan. Endpoints default to production and
/// can be overridden (tests point them at a local mock server).
#[derive(Debug, Clone)]
pub struct DhanConfig {
    pub client_id: String,
    pub access_token: Secret,
    /// REST base, including the version prefix.
    pub api_base: String,
    /// Host for access-token generation.
    pub auth_base: String,
    pub market_feed_url: String,
    pub order_update_url: String,
    /// Total time allowed for one HTTP request (connect + send + response).
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Retries for read-only (GET) calls on transient failures. Mutating
    /// calls are never retried.
    pub max_read_retries: u32,
    pub rate_limits: RateLimitConfig,
}

impl DhanConfig {
    pub const DEFAULT_API_BASE: &'static str = "https://api.dhan.co/v2";
    pub const DEFAULT_AUTH_BASE: &'static str = "https://auth.dhan.co";
    pub const DEFAULT_MARKET_FEED_URL: &'static str = "wss://api-feed.dhan.co";
    pub const DEFAULT_ORDER_UPDATE_URL: &'static str = "wss://api-order-update.dhan.co";

    pub fn new(client_id: impl Into<String>, access_token: impl Into<Secret>) -> Self {
        Self {
            client_id: client_id.into(),
            access_token: access_token.into(),
            api_base: Self::DEFAULT_API_BASE.to_owned(),
            auth_base: Self::DEFAULT_AUTH_BASE.to_owned(),
            market_feed_url: Self::DEFAULT_MARKET_FEED_URL.to_owned(),
            order_update_url: Self::DEFAULT_ORDER_UPDATE_URL.to_owned(),
            request_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            max_read_retries: 2,
            rate_limits: RateLimitConfig::default(),
        }
    }

    /// Reads `DHAN_CLIENT_ID` and `DHAN_ACCESS_TOKEN`.
    pub fn from_env() -> Result<Self> {
        let client_id = read_env("DHAN_CLIENT_ID")?;
        let token = read_env("DHAN_ACCESS_TOKEN")?;
        Ok(Self::new(client_id, Secret::new(token)))
    }

    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn with_auth_base(mut self, base: impl Into<String>) -> Self {
        self.auth_base = base.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn with_market_feed_url(mut self, url: impl Into<String>) -> Self {
        self.market_feed_url = url.into();
        self
    }

    pub fn with_order_update_url(mut self, url: impl Into<String>) -> Self {
        self.order_update_url = url.into();
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_max_read_retries(mut self, retries: u32) -> Self {
        self.max_read_retries = retries;
        self
    }

    pub fn with_rate_limits(mut self, limits: RateLimitConfig) -> Self {
        self.rate_limits = limits;
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.client_id.trim().is_empty() {
            return Err(DhanError::InvalidRequest("client_id is empty".into()));
        }
        if self.access_token.is_empty() {
            return Err(DhanError::InvalidRequest("access_token is empty".into()));
        }
        Ok(())
    }
}

fn read_env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| DhanError::InvalidRequest(format!("environment variable {name} is not set")))
}
