//! Access-token generation on `auth.dhan.co`.
//!
//! Tokens last 24 hours (SEBI rule), so a fresh one is needed every trading
//! day. Two flows are supported for individual accounts:
//!
//! 1. **PIN + TOTP** — fully automatic once TOTP is enabled on the account:
//!    [`DhanAuth::generate_token`] (or [`DhanAuth::generate_token_with_totp_secret`]
//!    to compute the TOTP code locally).
//! 2. **API key + secret** — three steps with a browser login in the middle:
//!    [`DhanAuth::generate_consent`] → open [`DhanAuth::consent_login_url`] →
//!    [`DhanAuth::consume_consent`] with the `tokenId` from the redirect.
//!
//! [`token::TokenKeeper`](crate::token::TokenKeeper) runs the first flow
//! automatically, so nobody has to log in each day.
//!
//! A token generated on web.dhan.co can be extended with
//! [`DhanClient::renew_token`](crate::DhanClient::renew_token).

use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::serde_util as su;
use crate::{DhanConfig, DhanError, Result, Secret};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_name: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_ucc: String,
    /// DDPI status.
    #[serde(default, deserialize_with = "su::bool_lenient")]
    pub given_power_of_attorney: bool,
    #[serde(default = "empty_secret", deserialize_with = "secret")]
    pub access_token: Secret,
    /// IST, e.g. `"2026-01-01T00:00:00.000"`.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub expiry_time: String,
}

fn empty_secret() -> Secret {
    Secret::new("")
}

fn secret<'de, D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
    su::string_lenient(d).map(Secret::new)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consent {
    #[serde(deserialize_with = "su::string_lenient")]
    pub consent_app_id: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub consent_app_status: String,
}

/// Client for `auth.dhan.co`. Needs no access token.
#[derive(Debug, Clone)]
pub struct DhanAuth {
    http: reqwest::Client,
    auth_base: String,
}

impl DhanAuth {
    pub fn new() -> Result<Self> {
        Self::with_base(DhanConfig::DEFAULT_AUTH_BASE)
    }

    pub fn with_base(auth_base: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(DhanError::transport)?;
        Ok(Self {
            http,
            auth_base: auth_base.into().trim_end_matches('/').to_owned(),
        })
    }

    /// PIN + TOTP flow. Requires TOTP to be set up on web.dhan.co.
    pub async fn generate_token(
        &self,
        client_id: &str,
        pin: &Secret,
        totp_code: &str,
    ) -> Result<TokenResponse> {
        if totp_code.len() != 6 || !totp_code.chars().all(|c| c.is_ascii_digit()) {
            return Err(DhanError::InvalidRequest(
                "TOTP code must be 6 digits".into(),
            ));
        }
        let url = format!("{}/app/generateAccessToken", self.auth_base);
        let req = self.http.post(url).query(&[
            ("dhanClientId", client_id),
            ("pin", pin.expose()),
            ("totp", totp_code),
        ]);
        let token: TokenResponse = send(req, "/app/generateAccessToken").await?;
        ensure_token(token, "/app/generateAccessToken")
    }

    /// PIN + TOTP flow, computing the TOTP code from the base32 secret shown
    /// when TOTP was enabled.
    #[cfg(feature = "totp")]
    pub async fn generate_token_with_totp_secret(
        &self,
        client_id: &str,
        pin: &Secret,
        totp_secret_base32: &Secret,
    ) -> Result<TokenResponse> {
        let code = totp_now(totp_secret_base32)?;
        self.generate_token(client_id, pin, &code).await
    }

    /// API-key flow, step 1: start a login session.
    pub async fn generate_consent(
        &self,
        client_id: &str,
        api_key: &str,
        api_secret: &Secret,
    ) -> Result<Consent> {
        let url = format!("{}/app/generate-consent", self.auth_base);
        let req = self
            .http
            .post(url)
            .query(&[("client_id", client_id)])
            .header("app_id", api_key)
            .header("app_secret", api_secret.expose());
        send(req, "/app/generate-consent").await
    }

    /// API-key flow, step 2: the user opens this URL and logs in. Dhan then
    /// redirects to the app's redirect URL with `?tokenId=...`.
    pub fn consent_login_url(&self, consent_app_id: &str) -> String {
        format!(
            "{}/login/consentApp-login?consentAppId={consent_app_id}",
            self.auth_base
        )
    }

    /// API-key flow, step 3: exchange the `tokenId` for an access token.
    pub async fn consume_consent(
        &self,
        token_id: &str,
        api_key: &str,
        api_secret: &Secret,
    ) -> Result<TokenResponse> {
        let url = format!("{}/app/consumeApp-consent", self.auth_base);
        let req = self
            .http
            .get(url)
            .query(&[("tokenId", token_id)])
            .header("app_id", api_key)
            .header("app_secret", api_secret.expose());
        let token: TokenResponse = send(req, "/app/consumeApp-consent").await?;
        ensure_token(token, "/app/consumeApp-consent")
    }
}

async fn send<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
    endpoint: &str,
) -> Result<T> {
    // The URL carries the PIN and TOTP code, so it must not reach any
    // error message.
    let resp = req
        .send()
        .await
        .map_err(|e| DhanError::transport(e.without_url()))?;
    let status = resp.status();
    let body = resp
        .bytes()
        .await
        .map_err(|e| DhanError::transport(e.without_url()))?;
    if !status.is_success() {
        return Err(DhanError::from_response(status.as_u16(), &body));
    }
    // Never echo token bodies into errors: drop the body on decode failure.
    serde_json::from_slice(&body).map_err(|e| DhanError::decode(endpoint, e, b"<redacted>"))
}

fn ensure_token(token: TokenResponse, endpoint: &str) -> Result<TokenResponse> {
    if token.access_token.is_empty() {
        return Err(DhanError::decode(
            endpoint,
            "no accessToken in response",
            b"",
        ));
    }
    Ok(token)
}

/// Current 6-digit TOTP code (RFC 6238, SHA-1, 30 s) for a base32 secret.
#[cfg(feature = "totp")]
pub fn totp_now(secret_base32: &Secret) -> Result<String> {
    use totp_rs::{Algorithm, Secret as TotpSecret, TOTP};

    let cleaned: String = secret_base32
        .expose()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let bytes = TotpSecret::Encoded(cleaned)
        .to_bytes()
        .map_err(|e| DhanError::Totp(format!("invalid base32 secret: {e:?}")))?;
    let totp = TOTP::new_unchecked(Algorithm::SHA1, 6, 1, 30, bytes);
    totp.generate_current()
        .map_err(|e| DhanError::Totp(e.to_string()))
}

#[cfg(all(test, feature = "totp"))]
mod tests {
    use super::*;

    #[test]
    fn totp_produces_six_digits() -> Result<()> {
        let code = totp_now(&Secret::new("JBSW Y3DP EHPK 3PXP"))?;
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        Ok(())
    }

    #[test]
    fn token_response_parses_and_hides_token() -> Result<(), serde_json::Error> {
        let t: TokenResponse = serde_json::from_str(
            r#"{"dhanClientId":"1000000401","dhanClientName":"JOHN DOE","dhanClientUcc":"ABCD12345E",
                "givenPowerOfAttorney":false,"accessToken":"eyJ.secret","expiryTime":"2026-01-01T00:00:00.000"}"#,
        )?;
        assert_eq!(t.access_token.expose(), "eyJ.secret");
        assert!(!format!("{t:?}").contains("eyJ.secret"));
        Ok(())
    }
}
