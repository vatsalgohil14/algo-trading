//! Fund limits and margin calculation.

use reqwest::Method;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::DhanClient;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{ExchangeSegment, ProductType, Result, SecurityId, TransactionType};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundLimit {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    /// Dhan spells this `availabelBalance`.
    #[serde(default, alias = "availabelBalance", deserialize_with = "su::decimal")]
    pub available_balance: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub sod_limit: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub collateral_amount: Decimal,
    /// Dhan spells this `receiveableAmount`.
    #[serde(default, alias = "receiveableAmount", deserialize_with = "su::decimal")]
    pub receivable_amount: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub utilized_amount: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub blocked_payout_amount: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub withdrawable_balance: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginRequest {
    pub exchange_segment: ExchangeSegment,
    pub transaction_type: TransactionType,
    pub quantity: u32,
    pub product_type: ProductType,
    pub security_id: SecurityId,
    #[serde(serialize_with = "su::ser_decimal")]
    pub price: Decimal,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub trigger_price: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginResponse {
    #[serde(default, deserialize_with = "su::decimal")]
    pub total_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub span_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub exposure_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub available_balance: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub variable_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub insufficient_balance: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub brokerage: Decimal,
    /// Leverage multiple for this product, e.g. 5 for 5x intraday.
    #[serde(default, deserialize_with = "su::decimal")]
    pub leverage: Decimal,
}

/// Request for the multi-order margin calculator.
///
/// Dhan's documentation shows two different spellings for this request;
/// this follows the endpoint's sample call (`includeOrder`, `scripList`).
/// Verify against a live call before relying on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiMarginRequest {
    pub include_position: bool,
    pub include_order: bool,
    pub scrip_list: Vec<MarginRequest>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MultiMarginResponse {
    #[serde(default, deserialize_with = "su::decimal")]
    pub total_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub span_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub exposure_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub equity_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub fo_margin: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub commodity_margin: Decimal,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub currency: String,
    #[serde(default, deserialize_with = "su::opt_decimal")]
    pub hedge_benefit: Option<Decimal>,
}

impl DhanClient {
    pub async fn fund_limit(&self) -> Result<FundLimit> {
        self.get("/fundlimit", ApiCategory::NonTrading).await
    }

    /// Margin, brokerage and leverage for one prospective order. Use this
    /// instead of assuming a fixed intraday leverage.
    pub async fn margin_for(&self, req: &MarginRequest) -> Result<MarginResponse> {
        self.send_json(
            Method::POST,
            "/margincalculator",
            req,
            true,
            ApiCategory::NonTrading,
        )
        .await
    }

    pub async fn margin_for_many(&self, req: &MultiMarginRequest) -> Result<MultiMarginResponse> {
        self.send_json(
            Method::POST,
            "/margincalculator/multi",
            req,
            true,
            ApiCategory::NonTrading,
        )
        .await
    }
}
