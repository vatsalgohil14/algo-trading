//! Keeps a valid access token without anyone logging in by hand.
//!
//! Tokens last 24 hours. Dhan issues a new one for the client id, PIN and a
//! TOTP code ([`DhanAuth::generate_token_with_totp_secret`]), so the
//! [`TokenKeeper`] does that by itself:
//!
//! * It keeps the token in a private file (owner-only, in the gitignored
//!   `secrets/` folder), so a restart does not ask for another one.
//! * A token must last to the end of the next trading day's session. The
//!   refresh therefore happens just after 16:00 IST, with hours to spare if
//!   it fails, and never during market hours in normal running.
//! * A failure that may clear up (network, server, rate limit) is retried
//!   with growing gaps. A refusal (such as a wrong PIN) stops at once,
//!   because repeated wrong attempts could lock the account.
//!
//! The PIN, TOTP secret and token never appear in logs or error messages.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDateTime, NaiveTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::auth::{DhanAuth, TokenResponse};
use crate::{DhanClient, DhanConfig, DhanError, ErrorKind, Result, Secret};

const IST_OFFSET_SECS: i32 = 5 * 3600 + 30 * 60;
/// A token must last until this IST hour, after the session has ended.
const SESSION_DONE_HOUR: u32 = 16;
/// Never ask again sooner than this after a success, whatever the expiry
/// looks like, so an unexpected expiry cannot cause a burst of requests.
const MIN_GAP_MINUTES: i64 = 5;
/// Refresh regardless of the schedule when this close to expiry.
const URGENT_MINUTES: i64 = 30;
/// Used if Dhan's expiry time cannot be read. Dhan documents 24 hours.
const ASSUMED_VALIDITY_HOURS: i64 = 23;
const MAX_RETRY_GAP: Duration = Duration::from_secs(30 * 60);

fn ist() -> Option<FixedOffset> {
    FixedOffset::east_opt(IST_OFFSET_SECS)
}

/// The next 16:00 IST at or after `now`: how long a token has to last.
pub fn required_until(now: DateTime<Utc>) -> DateTime<Utc> {
    let (Some(offset), Some(time)) = (ist(), NaiveTime::from_hms_opt(SESSION_DONE_HOUR, 0, 0))
    else {
        return now;
    };
    let local = now.with_timezone(&offset);
    let mut target = local.date_naive().and_time(time);
    if local.naive_local() > target {
        target += TimeDelta::days(1);
    }
    target
        .and_local_timezone(offset)
        .single()
        .map_or(now, |t| t.with_timezone(&Utc))
}

/// Whether a token expiring at `expires` should be replaced now.
pub fn refresh_due(
    expires: DateTime<Utc>,
    generated: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if generated.is_some_and(|g| now - g < TimeDelta::minutes(MIN_GAP_MINUTES)) {
        return false;
    }
    expires - now <= TimeDelta::minutes(URGENT_MINUTES) || expires < required_until(now)
}

/// Dhan's expiry time (IST, with or without fractions of a second).
fn parse_expiry(text: &str) -> Option<DateTime<Utc>> {
    let local = NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    local
        .and_local_timezone(ist()?)
        .single()
        .map(|t| t.with_timezone(&Utc))
}

/// A usable token and when it stops working.
#[derive(Debug, Clone)]
pub struct ValidToken {
    pub token: Secret,
    pub expires: DateTime<Utc>,
    generated: Option<DateTime<Utc>>,
}

impl ValidToken {
    /// A token with no record of when it was issued, for tests.
    #[doc(hidden)]
    pub fn for_testing(token: Secret, expires: DateTime<Utc>) -> Self {
        Self {
            token,
            expires,
            generated: None,
        }
    }
}

/// How the background refresh is doing. Alert on anything but `Healthy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStatus {
    Healthy {
        expires: DateTime<Utc>,
    },
    /// The last attempts failed; still trying.
    Retrying {
        failures: u32,
        reason: String,
    },
    /// Refused by Dhan. Needs a person; nothing more will be tried.
    Stopped {
        reason: String,
    },
}

/// Settings for a [`TokenKeeper`].
#[derive(Debug, Clone)]
pub struct KeeperConfig {
    /// Client settings. Its `access_token` is replaced by the kept one.
    pub client: DhanConfig,
    pub pin: Secret,
    pub totp_secret: Secret,
    pub cache_path: PathBuf,
    pub poll_every: Duration,
    pub first_retry_after: Duration,
    /// Wait for a new code if the current one has fewer seconds than this
    /// left, so that it cannot expire on the way.
    pub min_code_seconds: u64,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl KeeperConfig {
    pub fn new(client: DhanConfig, pin: Secret, totp_secret: Secret) -> Self {
        Self {
            client,
            pin,
            totp_secret,
            cache_path: PathBuf::from("secrets/dhan_token.json"),
            poll_every: Duration::from_secs(60),
            first_retry_after: Duration::from_secs(60),
            min_code_seconds: 5,
        }
    }

    /// Reads `DHAN_CLIENT_ID`, `DHAN_PIN` and `DHAN_TOTP_SECRET`, and
    /// optionally `DHAN_TOKEN_CACHE`.
    pub fn from_env() -> Result<Self> {
        let need = |name: &str| {
            env(name).ok_or_else(|| {
                DhanError::InvalidRequest(format!("environment variable {name} is not set"))
            })
        };
        let client = DhanConfig::new(need("DHAN_CLIENT_ID")?, Secret::new("not-yet-issued"));
        let mut config = Self::new(
            client,
            Secret::new(need("DHAN_PIN")?),
            Secret::new(need("DHAN_TOTP_SECRET")?),
        );
        if let Some(path) = env("DHAN_TOKEN_CACHE") {
            config.cache_path = PathBuf::from(path);
        }
        Ok(config)
    }
}

/// The token file. Deliberately not `Debug`.
#[derive(Serialize, Deserialize)]
struct Cached {
    client_id: String,
    access_token: String,
    expires_at: i64,
    generated_at: i64,
}

fn read_cache(path: &Path, client_id: &str) -> Option<ValidToken> {
    let text = std::fs::read_to_string(path).ok()?;
    let cached: Cached = serde_json::from_str(&text).ok()?;
    if cached.client_id != client_id || cached.access_token.trim().is_empty() {
        return None;
    }
    Some(ValidToken {
        token: Secret::new(cached.access_token),
        expires: DateTime::from_timestamp(cached.expires_at, 0)?,
        generated: DateTime::from_timestamp(cached.generated_at, 0),
    })
}

/// Writes the token where only this user can read it, replacing the old
/// file in one step so a crash cannot leave half a token behind.
fn write_cache(path: &Path, cached: &Cached) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let temporary = path.with_extension("tmp");
    let _ = std::fs::remove_file(&temporary);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec(cached)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

/// See the [module docs](self).
#[derive(Debug)]
pub struct TokenKeeper {
    config: KeeperConfig,
    auth: DhanAuth,
}

impl TokenKeeper {
    pub fn new(config: KeeperConfig) -> Result<Self> {
        let auth = DhanAuth::with_base(&config.client.auth_base)?;
        Ok(Self { config, auth })
    }

    pub fn from_env() -> Result<Self> {
        Self::new(KeeperConfig::from_env()?)
    }

    /// Asks Dhan for a new token and saves it.
    async fn generate(&self) -> Result<ValidToken> {
        let window = 30;
        let left = window - Utc::now().timestamp().rem_euclid(window as i64) as u64;
        if left < self.config.min_code_seconds {
            tokio::time::sleep(Duration::from_secs(left)).await;
        }
        let response: TokenResponse = self
            .auth
            .generate_token_with_totp_secret(
                &self.config.client.client_id,
                &self.config.pin,
                &self.config.totp_secret,
            )
            .await?;
        let now = Utc::now();
        let expires = parse_expiry(&response.expiry_time).unwrap_or_else(|| {
            tracing::warn!("could not read Dhan's token expiry; assuming about a day");
            now + TimeDelta::hours(ASSUMED_VALIDITY_HOURS)
        });
        let cached = Cached {
            client_id: self.config.client.client_id.clone(),
            access_token: response.access_token.expose().to_owned(),
            expires_at: expires.timestamp(),
            generated_at: now.timestamp(),
        };
        if let Err(e) = write_cache(&self.config.cache_path, &cached) {
            tracing::warn!("could not save the access token: {e}");
        }
        Ok(ValidToken {
            token: response.access_token,
            expires,
            generated: Some(now),
        })
    }

    /// A client with a working token: the saved one if it is still good,
    /// otherwise a new one.
    pub async fn connect(&self) -> Result<(DhanClient, ValidToken)> {
        let saved = read_cache(&self.config.cache_path, &self.config.client.client_id)
            .filter(|t| !refresh_due(t.expires, t.generated, Utc::now()));
        if let Some(valid) = saved {
            let client = self.client_with(&valid.token)?;
            match client.profile().await {
                Err(e) if e.kind() == ErrorKind::Auth => {
                    tracing::warn!("the saved access token was refused; getting a new one");
                }
                // Anything else says nothing about the token itself.
                _ => return Ok((client, valid)),
            }
        }
        let valid = self.generate().await?;
        Ok((self.client_with(&valid.token)?, valid))
    }

    fn client_with(&self, token: &Secret) -> Result<DhanClient> {
        DhanClient::new(DhanConfig {
            access_token: token.clone(),
            ..self.config.client.clone()
        })
    }

    /// Keeps `client` supplied with a valid token in the background, until
    /// the returned handle is dropped.
    pub fn spawn_refresh(self, client: DhanClient, current: &ValidToken) -> RefreshHandle {
        let (sender, status) = watch::channel(TokenStatus::Healthy {
            expires: current.expires,
        });
        let current = current.clone();
        let task = tokio::spawn(async move {
            self.refresh_loop(client, current, sender).await;
        });
        RefreshHandle { status, task }
    }

    async fn refresh_loop(
        self,
        client: DhanClient,
        mut current: ValidToken,
        status: watch::Sender<TokenStatus>,
    ) {
        let mut failures: u32 = 0;
        let mut retry_at: Option<Instant> = None;
        loop {
            tokio::time::sleep(self.config.poll_every).await;
            if !refresh_due(current.expires, current.generated, Utc::now())
                || retry_at.is_some_and(|t| Instant::now() < t)
            {
                continue;
            }
            match self.generate().await {
                Ok(fresh) => {
                    client.set_access_token(fresh.token.clone());
                    let _ = status.send(TokenStatus::Healthy {
                        expires: fresh.expires,
                    });
                    tracing::info!("access token renewed, valid until {}", fresh.expires);
                    current = fresh;
                    failures = 0;
                    retry_at = None;
                }
                Err(e) if e.is_retryable() => {
                    failures = failures.saturating_add(1);
                    let gap = self
                        .config
                        .first_retry_after
                        .saturating_mul(2u32.saturating_pow(failures - 1))
                        .min(MAX_RETRY_GAP);
                    retry_at = Some(Instant::now() + gap);
                    tracing::warn!("could not renew the access token (attempt {failures}): {e}");
                    let _ = status.send(TokenStatus::Retrying {
                        failures,
                        reason: e.to_string(),
                    });
                }
                Err(e) => {
                    tracing::error!("Dhan refused the token request, not retrying: {e}");
                    let _ = status.send(TokenStatus::Stopped {
                        reason: e.to_string(),
                    });
                    return;
                }
            }
        }
    }
}

/// The running background refresh. Dropping it stops the refresh.
#[derive(Debug)]
pub struct RefreshHandle {
    status: watch::Receiver<TokenStatus>,
    task: JoinHandle<()>,
}

impl RefreshHandle {
    pub fn status(&self) -> TokenStatus {
        self.status.borrow().clone()
    }

    /// A receiver that wakes on every change of status.
    pub fn watch(&self) -> watch::Receiver<TokenStatus> {
        self.status.clone()
    }
}

impl Drop for RefreshHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A client from the environment: automatic tokens if `DHAN_PIN` and
/// `DHAN_TOTP_SECRET` are set, otherwise `DHAN_ACCESS_TOKEN`.
pub async fn connect_from_env() -> Result<DhanClient> {
    if env("DHAN_PIN").is_some() && env("DHAN_TOTP_SECRET").is_some() {
        Ok(TokenKeeper::from_env()?.connect().await?.0)
    } else {
        DhanClient::from_env()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A moment given in IST.
    fn at(text: &str) -> DateTime<Utc> {
        parse_expiry(text).unwrap()
    }

    #[test]
    fn reads_dhan_expiry_times() {
        let a = at("2026-01-01T00:00:00.000");
        assert_eq!(a.to_rfc3339(), "2025-12-31T18:30:00+00:00");
        assert_eq!(at("2025-09-23T12:37:23").timestamp() % 60, 23);
        assert!(parse_expiry("soon").is_none());
    }

    #[test]
    fn a_token_must_last_to_the_end_of_the_session() {
        assert_eq!(
            required_until(at("2026-01-05T10:00:00")),
            at("2026-01-05T16:00:00")
        );
        assert_eq!(
            required_until(at("2026-01-05T16:00:00")),
            at("2026-01-05T16:00:00")
        );
        assert_eq!(
            required_until(at("2026-01-05T16:00:01")),
            at("2026-01-06T16:00:00")
        );
    }

    #[test]
    fn refresh_schedule() {
        let due = |expires: &str, now: &str| refresh_due(at(expires), None, at(now));
        // Issued 16:01 yesterday: lasts through today's session.
        assert!(!due("2026-01-06T16:01:00", "2026-01-05T17:00:00"));
        assert!(!due("2026-01-06T16:01:00", "2026-01-06T15:30:00"));
        // Once today's session is over, tomorrow's needs a longer one.
        assert!(due("2026-01-06T16:01:00", "2026-01-06T16:00:30"));
        // Issued mid-morning: still good today, replaced after the close.
        assert!(!due("2026-01-07T10:00:00", "2026-01-06T15:59:00"));
        assert!(due("2026-01-07T10:00:00", "2026-01-06T16:01:00"));
        // Expiring today before the session ends.
        assert!(due("2026-01-06T12:00:00", "2026-01-06T09:00:00"));
        // Already expired.
        assert!(due("2026-01-05T12:00:00", "2026-01-06T09:00:00"));
    }

    #[test]
    fn never_asks_again_right_after_a_success() {
        let now = at("2026-01-06T09:00:00");
        let just_issued = Some(now - TimeDelta::minutes(1));
        assert!(!refresh_due(now, just_issued, now));
        let later = Some(now - TimeDelta::minutes(MIN_GAP_MINUTES + 1));
        assert!(refresh_due(now, later, now));
    }
}
