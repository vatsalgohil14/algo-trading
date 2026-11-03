//! Super orders: entry + target + stop-loss (optionally trailing) in one
//! request. The exit legs live at the broker, so a position stays protected
//! even if this process dies — the main reason to prefer them for intraday.

use chrono::NaiveDateTime;
use reqwest::Method;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::DhanClient;
use super::orders::{OrderAck, validate_correlation_id};
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{
    DhanError, ExchangeSegment, LegName, OrderStatus, OrderType, ProductType, Result, SecurityId,
    TransactionType, Validity,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceSuperOrderRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub transaction_type: TransactionType,
    pub exchange_segment: ExchangeSegment,
    pub product_type: ProductType,
    /// `Limit` or `Market` only.
    pub order_type: OrderType,
    pub security_id: SecurityId,
    pub quantity: u32,
    /// Entry price; zero for a market entry.
    #[serde(serialize_with = "su::ser_decimal")]
    pub price: Decimal,
    #[serde(serialize_with = "su::ser_decimal")]
    pub target_price: Decimal,
    #[serde(serialize_with = "su::ser_decimal")]
    pub stop_loss_price: Decimal,
    /// Trail the stop by this much per favourable move; zero disables trailing.
    #[serde(serialize_with = "su::ser_decimal")]
    pub trailing_jump: Decimal,
}

impl PlaceSuperOrderRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn limit(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        price: Decimal,
        target_price: Decimal,
        stop_loss_price: Decimal,
        product: ProductType,
    ) -> Self {
        Self {
            correlation_id: None,
            transaction_type: side,
            exchange_segment: segment,
            product_type: product,
            order_type: OrderType::Limit,
            security_id,
            quantity,
            price,
            target_price,
            stop_loss_price,
            trailing_jump: Decimal::ZERO,
        }
    }

    pub fn market(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        target_price: Decimal,
        stop_loss_price: Decimal,
        product: ProductType,
    ) -> Self {
        Self {
            order_type: OrderType::Market,
            price: Decimal::ZERO,
            ..Self::limit(
                side,
                segment,
                security_id,
                quantity,
                Decimal::ZERO,
                target_price,
                stop_loss_price,
                product,
            )
        }
    }

    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    pub fn with_trailing_jump(mut self, jump: Decimal) -> Self {
        self.trailing_jump = jump;
        self
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(DhanError::InvalidRequest(m.to_owned()));
        if let Some(id) = &self.correlation_id {
            validate_correlation_id(id)?;
        }
        if self.quantity == 0 {
            return bad("quantity must be > 0");
        }
        if !matches!(
            self.product_type,
            ProductType::Cnc | ProductType::Intraday | ProductType::Margin | ProductType::Mtf
        ) {
            return bad("super orders support CNC, INTRADAY, MARGIN and MTF only");
        }
        match self.order_type {
            OrderType::Limit if self.price <= Decimal::ZERO => {
                return bad("LIMIT entry needs a price");
            }
            OrderType::Market if self.price != Decimal::ZERO => {
                return bad("MARKET entry must not carry a price");
            }
            OrderType::Limit | OrderType::Market => {}
            _ => return bad("super orders support LIMIT and MARKET entries only"),
        }
        if self.target_price <= Decimal::ZERO || self.stop_loss_price <= Decimal::ZERO {
            return bad("target and stop-loss prices must be positive");
        }
        if self.trailing_jump < Decimal::ZERO {
            return bad("trailing jump cannot be negative");
        }
        // For a long the stop sits below the target; for a short, above.
        let ordered = match self.transaction_type {
            TransactionType::Buy => self.stop_loss_price < self.target_price,
            TransactionType::Sell => self.stop_loss_price > self.target_price,
        };
        if !ordered {
            return bad("stop-loss and target are on the wrong sides for this direction");
        }
        if self.order_type == OrderType::Limit {
            let inside = match self.transaction_type {
                TransactionType::Buy => {
                    self.stop_loss_price < self.price && self.price < self.target_price
                }
                TransactionType::Sell => {
                    self.target_price < self.price && self.price < self.stop_loss_price
                }
            };
            if !inside {
                return bad("entry price must lie between stop-loss and target");
            }
        }
        Ok(())
    }
}

/// Change one leg of a super order. Use the constructor matching the leg.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifySuperOrderRequest {
    pub order_id: String,
    pub leg_name: LegName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_type: Option<OrderType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<u32>,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub price: Option<Decimal>,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub target_price: Option<Decimal>,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_loss_price: Option<Decimal>,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub trailing_jump: Option<Decimal>,
}

impl ModifySuperOrderRequest {
    /// Modify the whole super order. Allowed only while the entry is
    /// `PENDING` or `PART_TRADED`.
    #[allow(clippy::too_many_arguments)]
    pub fn entry(
        order_id: impl Into<String>,
        order_type: OrderType,
        quantity: u32,
        price: Decimal,
        target_price: Decimal,
        stop_loss_price: Decimal,
        trailing_jump: Decimal,
    ) -> Self {
        Self {
            order_id: order_id.into(),
            leg_name: LegName::EntryLeg,
            order_type: Some(order_type),
            quantity: Some(quantity),
            price: Some(price),
            target_price: Some(target_price),
            stop_loss_price: Some(stop_loss_price),
            trailing_jump: Some(trailing_jump),
        }
    }

    pub fn target(order_id: impl Into<String>, target_price: Decimal) -> Self {
        Self {
            order_id: order_id.into(),
            leg_name: LegName::TargetLeg,
            order_type: None,
            quantity: None,
            price: None,
            target_price: Some(target_price),
            stop_loss_price: None,
            trailing_jump: None,
        }
    }

    /// Passing a zero `trailing_jump` cancels trailing.
    pub fn stop_loss(
        order_id: impl Into<String>,
        stop_loss_price: Decimal,
        trailing_jump: Decimal,
    ) -> Self {
        Self {
            order_id: order_id.into(),
            leg_name: LegName::StopLossLeg,
            order_type: None,
            quantity: None,
            price: None,
            target_price: None,
            stop_loss_price: Some(stop_loss_price),
            trailing_jump: Some(trailing_jump),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperOrderLeg {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub order_id: String,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub leg_name: Option<LegName>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub transaction_type: Option<TransactionType>,
    /// Dhan spells this `totalQuatity`.
    #[serde(default, alias = "totalQuatity", deserialize_with = "su::i64_lenient")]
    pub total_quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub triggered_quantity: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub price: Decimal,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub order_status: OrderStatus,
    #[serde(default, deserialize_with = "su::decimal")]
    pub trailing_jump: Decimal,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperOrder {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    #[serde(deserialize_with = "su::string_lenient")]
    pub order_id: String,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub correlation_id: Option<String>,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub order_status: OrderStatus,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub transaction_type: Option<TransactionType>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub exchange_segment: Option<ExchangeSegment>,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub product_type: ProductType,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub order_type: OrderType,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub validity: Validity,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub trading_symbol: Option<String>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub security_id: Option<SecurityId>,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub ltp: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub price: Decimal,
    #[serde(default, deserialize_with = "su::bool_lenient")]
    pub after_market_order: bool,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub leg_name: Option<LegName>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub exchange_order_id: Option<String>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub create_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub update_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub exchange_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub oms_error_description: Option<String>,
    #[serde(default, deserialize_with = "su::decimal")]
    pub average_traded_price: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub filled_qty: i64,
    #[serde(default)]
    pub leg_details: Vec<SuperOrderLeg>,
}

impl DhanClient {
    /// Places a super order. Never retried automatically (see
    /// [`place_order`](Self::place_order)).
    pub async fn place_super_order(&self, req: &PlaceSuperOrderRequest) -> Result<OrderAck> {
        req.validate()?;
        tracing::info!(
            security_id = %req.security_id, side = ?req.transaction_type, qty = req.quantity,
            target = %req.target_price, stop = %req.stop_loss_price,
            correlation_id = ?req.correlation_id, "placing super order"
        );
        self.send_json(Method::POST, "/super/orders", req, true, ApiCategory::Order)
            .await
    }

    pub async fn modify_super_order(&self, req: &ModifySuperOrderRequest) -> Result<OrderAck> {
        if req.order_id.trim().is_empty() {
            return Err(DhanError::InvalidRequest("order id is empty".into()));
        }
        let path = format!("/super/orders/{}", req.order_id);
        self.send_json(Method::PUT, &path, req, true, ApiCategory::Order)
            .await
    }

    /// Cancelling `EntryLeg` cancels the whole super order. A cancelled
    /// target or stop-loss leg cannot be added back.
    pub async fn cancel_super_order(
        &self,
        order_id: &str,
        leg: LegName,
    ) -> Result<Option<OrderAck>> {
        if order_id.trim().is_empty() {
            return Err(DhanError::InvalidRequest("order id is empty".into()));
        }
        let path = format!("/super/orders/{order_id}/{}", leg.as_str());
        self.send_json_opt::<(), _>(Method::DELETE, &path, None, &[], false, ApiCategory::Order)
            .await
    }

    pub async fn super_order_book(&self) -> Result<Vec<SuperOrder>> {
        let v = self
            .get_value("/super/orders", ApiCategory::NonTrading)
            .await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/super/orders", e, b""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(v: i64) -> Decimal {
        Decimal::new(v, 0)
    }

    #[test]
    fn checks_leg_ordering() {
        let ok = PlaceSuperOrderRequest::limit(
            TransactionType::Buy,
            ExchangeSegment::NseEq,
            SecurityId(11536),
            5,
            d(1500),
            d(1600),
            d(1400),
            ProductType::Intraday,
        );
        assert!(ok.validate().is_ok());

        let mut flipped = ok.clone();
        flipped.transaction_type = TransactionType::Sell;
        assert!(flipped.validate().is_err());

        let short = PlaceSuperOrderRequest::market(
            TransactionType::Sell,
            ExchangeSegment::NseEq,
            SecurityId(11536),
            5,
            d(1400),
            d(1600),
            ProductType::Intraday,
        );
        assert!(short.validate().is_ok());
    }
}
