//! Profile, token renewal and static-IP management.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::DhanClient;
use crate::auth::TokenResponse;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{DhanError, Result};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    /// e.g. `"30/03/2025 15:37"`.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub token_validity: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub active_segment: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub ddpi: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub mtf: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub data_plan: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub data_validity: String,
}

impl Profile {
    pub fn has_data_plan(&self) -> bool {
        self.data_plan.eq_ignore_ascii_case("active")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IpFlag {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetIpBody<'a> {
    ip: &'a str,
    ip_flag: IpFlag,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticIps {
    #[serde(default, rename = "primaryIP", deserialize_with = "su::opt_string")]
    pub primary_ip: Option<String>,
    #[serde(default, rename = "secondaryIP", deserialize_with = "su::opt_string")]
    pub secondary_ip: Option<String>,
    /// Date from which the primary IP may be changed (YYYY-MM-DD).
    #[serde(default, deserialize_with = "su::opt_string")]
    pub modify_date_primary: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub modify_date_secondary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct IpSetResult {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub message: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub status: String,
}

impl DhanClient {
    /// Cheapest authenticated call; use it to check that the token works.
    pub async fn profile(&self) -> Result<Profile> {
        self.get("/profile", ApiCategory::NonTrading).await
    }

    /// Extends a web-generated token by 24 hours. The old token stops
    /// working; this client switches to the new one automatically. Hand the
    /// result to the WebSocket clients as well.
    pub async fn renew_token(&self) -> Result<TokenResponse> {
        let url = format!("{}/RenewToken", self.config().api_base);
        let token = self.access_token();
        let resp = self
            .http()
            .get(url)
            .header("access-token", token.expose())
            .header("dhanClientId", self.client_id())
            .send()
            .await
            .map_err(DhanError::transport)?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(DhanError::transport)?;
        if !status.is_success() {
            return Err(DhanError::from_response(status.as_u16(), &body));
        }
        let parsed: TokenResponse = serde_json::from_slice(&body)
            .map_err(|e| DhanError::decode("/RenewToken", e, &body))?;
        if parsed.access_token.is_empty() {
            return Err(DhanError::decode(
                "/RenewToken",
                "no accessToken in response",
                &body,
            ));
        }
        self.set_access_token(parsed.access_token.clone());
        tracing::info!(expiry = %parsed.expiry_time, "access token renewed");
        Ok(parsed)
    }

    /// Whitelists a static IP. Once set it cannot be changed for 7 days.
    pub async fn set_static_ip(&self, ip: &str, flag: IpFlag) -> Result<IpSetResult> {
        self.ip_call(Method::POST, "/ip/setIP", ip, flag).await
    }

    pub async fn modify_static_ip(&self, ip: &str, flag: IpFlag) -> Result<IpSetResult> {
        self.ip_call(Method::PUT, "/ip/modifyIP", ip, flag).await
    }

    pub async fn static_ips(&self) -> Result<StaticIps> {
        self.get("/ip/getIP", ApiCategory::NonTrading).await
    }

    async fn ip_call(
        &self,
        method: Method,
        path: &str,
        ip: &str,
        flag: IpFlag,
    ) -> Result<IpSetResult> {
        if ip.parse::<std::net::IpAddr>().is_err() {
            return Err(DhanError::InvalidRequest(format!(
                "{ip:?} is not an IP address"
            )));
        }
        let body = SetIpBody { ip, ip_flag: flag };
        self.send_json(method, path, &body, true, ApiCategory::NonTrading)
            .await
    }
}
