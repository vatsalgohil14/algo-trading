//! Holdings, positions, position conversion and exit-all.

use reqwest::Method;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::DhanClient;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{DhanError, ExchangeSegment, PositionType, ProductType, Result, SecurityId};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Holding {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub exchange: String,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub trading_symbol: Option<String>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub security_id: Option<SecurityId>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub isin: Option<String>,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub total_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub dp_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub t1_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub available_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub collateral_qty: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub avg_cost_price: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub trading_symbol: Option<String>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub security_id: Option<SecurityId>,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub position_type: PositionType,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub exchange_segment: Option<ExchangeSegment>,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub product_type: ProductType,
    #[serde(default, deserialize_with = "su::decimal")]
    pub buy_avg: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub buy_qty: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub cost_price: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub sell_avg: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub sell_qty: i64,
    /// Positive = long, negative = short.
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub net_qty: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub realized_profit: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub unrealized_profit: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub rbi_reference_rate: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub multiplier: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub carry_forward_buy_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub carry_forward_sell_qty: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub carry_forward_buy_value: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub carry_forward_sell_value: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub day_buy_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub day_sell_qty: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub day_buy_value: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub day_sell_value: Decimal,
    #[serde(default, deserialize_with = "su::bool_lenient")]
    pub cross_currency: bool,
}

impl Position {
    pub fn is_open(&self) -> bool {
        self.net_qty != 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertPositionRequest {
    pub from_product_type: ProductType,
    pub to_product_type: ProductType,
    pub exchange_segment: ExchangeSegment,
    pub position_type: PositionType,
    pub security_id: SecurityId,
    pub convert_qty: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StatusMessage {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub status: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub message: String,
}

impl DhanClient {
    pub async fn holdings(&self) -> Result<Vec<Holding>> {
        let v = self.get_value("/holdings", ApiCategory::NonTrading).await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/holdings", e, b""))
    }

    /// Open positions for the day (plus F&O carry-forward).
    pub async fn positions(&self) -> Result<Vec<Position>> {
        let v = self
            .get_value("/positions", ApiCategory::NonTrading)
            .await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/positions", e, b""))
    }

    /// Converts a position between product types, e.g. INTRADAY → CNC.
    pub async fn convert_position(&self, req: &ConvertPositionRequest) -> Result<()> {
        if req.convert_qty == 0 {
            return Err(DhanError::InvalidRequest("convert_qty must be > 0".into()));
        }
        self.send_json_opt::<_, serde_json::Value>(
            Method::POST,
            "/positions/convert",
            Some(req),
            &[],
            true,
            ApiCategory::NonTrading,
        )
        .await
        .map(|_| ())
    }

    /// Exits every open position and cancels every open order for the day.
    /// This is the emergency flatten button.
    pub async fn exit_all_positions(&self) -> Result<StatusMessage> {
        tracing::warn!("exiting all positions and cancelling all orders");
        let resp = self
            .send_json_opt::<(), StatusMessage>(
                Method::DELETE,
                "/positions",
                None,
                &[],
                false,
                ApiCategory::Order,
            )
            .await?;
        Ok(resp.unwrap_or(StatusMessage {
            status: "ACCEPTED".into(),
            message: String::new(),
        }))
    }
}
