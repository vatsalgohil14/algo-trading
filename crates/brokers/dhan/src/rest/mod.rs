//! REST client. [`DhanClient`] holds the HTTP connection pool, the current
//! access token and the rate limiters; each submodule adds the methods for
//! one area of the API.

mod account;
mod funds;
mod historical;
mod market;
mod orders;
mod portfolio;
mod statements;
mod super_orders;
mod traders_control;

pub use account::*;
pub use funds::*;
pub use historical::*;
pub use market::*;
pub use orders::*;
pub use portfolio::*;
pub use statements::*;
pub use super_orders::*;
pub use traders_control::*;

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use reqwest::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::rate_limit::{ApiCategory, RateLimiters};
use crate::{DhanConfig, DhanError, Result, Secret};

/// Cheap-to-clone handle to the Dhan REST API. Clones share the connection
/// pool, token and rate limiters, so use one client per account.
#[derive(Debug, Clone)]
pub struct DhanClient {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    config: DhanConfig,
    token: ArcSwap<Secret>,
    limits: RateLimiters,
}

/// Request body wrapper that adds `dhanClientId`, which most endpoints need.
#[derive(Serialize)]
struct WithClientId<'a, T: Serialize> {
    #[serde(rename = "dhanClientId")]
    client_id: &'a str,
    #[serde(flatten)]
    inner: &'a T,
}

impl DhanClient {
    pub fn new(config: DhanConfig) -> Result<Self> {
        config.validate()?;
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .connect_timeout(config.connect_timeout)
            .tcp_nodelay(true)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(DhanError::transport)?;
        let limits = RateLimiters::new(&config.rate_limits)?;
        let token = ArcSwap::from_pointee(config.access_token.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                config,
                token,
                limits,
            }),
        })
    }

    /// Builds a client from `DHAN_CLIENT_ID` and `DHAN_ACCESS_TOKEN`.
    pub fn from_env() -> Result<Self> {
        Self::new(DhanConfig::from_env()?)
    }

    pub fn client_id(&self) -> &str {
        &self.inner.config.client_id
    }

    pub fn config(&self) -> &DhanConfig {
        &self.inner.config
    }

    /// Swaps in a new access token. In-flight requests finish with the old
    /// one; every later request uses the new one.
    pub fn set_access_token(&self, token: Secret) {
        self.inner.token.store(Arc::new(token));
    }

    /// The token currently in use (for handing to the WebSocket clients).
    pub fn access_token(&self) -> Secret {
        Secret::clone(&self.inner.token.load())
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    // ---- request helpers used by the submodules ----

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str, cat: ApiCategory) -> Result<T> {
        let body = self.execute(Method::GET, path, &[], None, cat).await?;
        decode(path, &body)
    }

    pub(crate) async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cat: ApiCategory,
    ) -> Result<T> {
        let body = self.execute(Method::GET, path, query, None, cat).await?;
        decode(path, &body)
    }

    /// GET returning raw JSON, for endpoints whose shape varies.
    pub(crate) async fn get_value(
        &self,
        path: &str,
        cat: ApiCategory,
    ) -> Result<serde_json::Value> {
        self.get(path, cat).await
    }

    pub(crate) async fn send_json<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        with_client_id: bool,
        cat: ApiCategory,
    ) -> Result<T> {
        let bytes = self.encode(body, with_client_id)?;
        let resp = self.execute(method, path, &[], Some(bytes), cat).await?;
        decode(path, &resp)
    }

    /// Like [`send_json`](Self::send_json) for endpoints that may answer
    /// `202 Accepted` with an empty body; returns `None` in that case.
    pub(crate) async fn send_json_opt<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        query: &[(&str, String)],
        with_client_id: bool,
        cat: ApiCategory,
    ) -> Result<Option<T>> {
        let bytes = match body {
            Some(b) => Some(self.encode(b, with_client_id)?),
            None => None,
        };
        let resp = self.execute(method, path, query, bytes, cat).await?;
        if resp.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        decode(path, &resp).map(Some)
    }

    fn encode<B: Serialize>(&self, body: &B, with_client_id: bool) -> Result<Vec<u8>> {
        let res = if with_client_id {
            serde_json::to_vec(&WithClientId {
                client_id: self.client_id(),
                inner: body,
            })
        } else {
            serde_json::to_vec(body)
        };
        res.map_err(|e| DhanError::InvalidRequest(format!("could not encode request: {e}")))
    }

    /// Sends one logical request: rate limit, send, map errors. GETs are
    /// retried on transient failures; other methods are sent exactly once.
    async fn execute(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Vec<u8>>,
        cat: ApiCategory,
    ) -> Result<Vec<u8>> {
        let idempotent = method == Method::GET;
        let max_attempts = if idempotent {
            1 + self.inner.config.max_read_retries
        } else {
            1
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.inner.limits.acquire(cat).await?;
            match self.send_once(&method, path, query, body.as_deref()).await {
                Ok(b) => return Ok(b),
                Err(e) if attempt < max_attempts && e.is_retryable() => {
                    let delay = Duration::from_millis(200 * 2u64.pow(attempt - 1));
                    tracing::warn!(%path, attempt, ?delay, error = %e, "retrying read-only Dhan call");
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn send_once(
        &self,
        method: &Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let url = format!("{}{}", self.inner.config.api_base, path);
        let token = self.inner.token.load();
        let mut req = self
            .inner
            .http
            .request(method.clone(), url)
            .header("access-token", token.expose())
            .header("client-id", self.client_id())
            .header(reqwest::header::ACCEPT, "application/json");
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(b.to_vec());
        }

        let started = std::time::Instant::now();
        let resp = req.send().await.map_err(DhanError::transport)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|source| DhanError::Transport {
            source,
            // The server already answered, so the request was processed.
            outcome_unknown: !status.is_success(),
        })?;
        tracing::debug!(%method, %path, status = status.as_u16(), elapsed_ms = started.elapsed().as_millis() as u64, "dhan call");

        if status.is_success() {
            Ok(bytes.to_vec())
        } else {
            Err(DhanError::from_response(status.as_u16(), &bytes))
        }
    }
}

fn decode<T: DeserializeOwned>(endpoint: &str, body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| DhanError::decode(endpoint, e, body))
}
