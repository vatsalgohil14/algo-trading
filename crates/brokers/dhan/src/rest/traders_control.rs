//! Broker-side risk controls: kill switch and P&L-based auto exit.
//!
//! These run at Dhan, independent of this process, so they are the last line
//! of defence if the trading system itself fails.

use reqwest::Method;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::DhanClient;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{DhanError, Result};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillSwitchStatus {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    /// `"ACTIVATE"` / `"DEACTIVATE"` on status checks, a sentence on changes.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub kill_switch_status: String,
}

impl KillSwitchStatus {
    pub fn is_active(&self) -> bool {
        let s = self.kill_switch_status.to_ascii_uppercase();
        s == "ACTIVATE" || s.contains("ACTIVATED") && !s.contains("DEACTIVATED")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PnlExitProduct {
    Intraday,
    Delivery,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PnlExitRequest {
    /// Exit everything once total profit reaches this.
    #[serde(serialize_with = "su::ser_decimal")]
    pub profit_value: Decimal,
    /// Exit everything once total loss reaches this (a positive amount).
    #[serde(serialize_with = "su::ser_decimal")]
    pub loss_value: Decimal,
    pub product_type: Vec<PnlExitProduct>,
    /// Also activate the kill switch after exiting.
    pub enable_kill_switch: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PnlExitStatus {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub pnl_exit_status: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub message: String,
    #[serde(default, deserialize_with = "su::opt_decimal")]
    pub profit: Option<Decimal>,
    #[serde(default, deserialize_with = "su::opt_decimal")]
    pub loss: Option<Decimal>,
    #[serde(default)]
    pub product_type: Vec<String>,
    #[serde(
        default,
        alias = "enable_kill_switch",
        deserialize_with = "su::bool_lenient"
    )]
    pub enable_kill_switch: bool,
}

impl DhanClient {
    /// Activates or deactivates the kill switch, which blocks all trading
    /// for the rest of the day. Dhan requires all positions to be closed and
    /// no orders pending before it can be activated.
    pub async fn set_kill_switch(&self, active: bool) -> Result<KillSwitchStatus> {
        let value = if active { "ACTIVATE" } else { "DEACTIVATE" };
        tracing::warn!(kill_switch = value, "changing kill switch");
        self.send_json_opt::<(), KillSwitchStatus>(
            Method::POST,
            "/killswitch",
            None,
            &[("killSwitchStatus", value.to_owned())],
            false,
            ApiCategory::NonTrading,
        )
        .await?
        .ok_or_else(|| DhanError::decode("/killswitch", "empty response", b""))
    }

    pub async fn kill_switch_status(&self) -> Result<KillSwitchStatus> {
        self.get("/killswitch", ApiCategory::NonTrading).await
    }

    /// Sets a broker-side P&L exit for today. If the thresholds are already
    /// breached when this is called, Dhan exits immediately.
    pub async fn set_pnl_exit(&self, req: &PnlExitRequest) -> Result<PnlExitStatus> {
        if req.profit_value <= Decimal::ZERO || req.loss_value <= Decimal::ZERO {
            return Err(DhanError::InvalidRequest(
                "profit and loss thresholds must be positive amounts".into(),
            ));
        }
        if req.product_type.is_empty() {
            return Err(DhanError::InvalidRequest(
                "choose at least one product type".into(),
            ));
        }
        self.send_json(
            Method::POST,
            "/pnlExit",
            req,
            false,
            ApiCategory::NonTrading,
        )
        .await
    }

    pub async fn stop_pnl_exit(&self) -> Result<PnlExitStatus> {
        self.send_json_opt::<(), PnlExitStatus>(
            Method::DELETE,
            "/pnlExit",
            None,
            &[],
            false,
            ApiCategory::NonTrading,
        )
        .await?
        .ok_or_else(|| DhanError::decode("/pnlExit", "empty response", b""))
    }

    pub async fn pnl_exit_status(&self) -> Result<PnlExitStatus> {
        self.get("/pnlExit", ApiCategory::NonTrading).await
    }
}
