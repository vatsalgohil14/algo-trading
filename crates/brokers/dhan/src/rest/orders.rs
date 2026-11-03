//! Regular orders: place, modify, cancel, slice, order book and trade book.
//!
//! Placing, modifying and cancelling require the calling machine's static IP
//! to be whitelisted with Dhan (SEBI rule). Dhan caps modifications at 25 per
//! order.

use chrono::NaiveDateTime;
use reqwest::Method;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::DhanClient;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{
    AmoTime, DhanError, ExchangeSegment, LegName, OrderStatus, OrderType, ProductType, Result,
    SecurityId, TransactionType, Validity,
};

/// Checks a correlation ID against Dhan's rules: 1-30 chars of
/// `[A-Za-z0-9 _-]`.
pub fn validate_correlation_id(id: &str) -> Result<()> {
    let ok_len = !id.is_empty() && id.len() <= 30;
    let ok_chars = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-');
    if ok_len && ok_chars {
        Ok(())
    } else {
        Err(DhanError::InvalidRequest(format!(
            "correlation id {id:?} must be 1-30 chars of [A-Za-z0-9 _-]"
        )))
    }
}

/// A new order. Build it with one of the constructors, then chain `with_*`.
///
/// ```
/// use dhan::{PlaceOrderRequest, ExchangeSegment, ProductType, SecurityId, TransactionType};
/// use rust_decimal::Decimal;
///
/// let req = PlaceOrderRequest::limit(
///     TransactionType::Buy,
///     ExchangeSegment::NseEq,
///     SecurityId(2885),
///     10,
///     Decimal::new(142_805, 2), // ₹1428.05
///     ProductType::Intraday,
/// )
/// .with_correlation_id("orb-2885-0001");
/// assert!(req.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceOrderRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub transaction_type: TransactionType,
    pub exchange_segment: ExchangeSegment,
    pub product_type: ProductType,
    pub order_type: OrderType,
    pub validity: Validity,
    pub security_id: SecurityId,
    pub quantity: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosed_quantity: Option<u32>,
    /// Limit price. Zero for market orders.
    #[serde(serialize_with = "su::ser_decimal")]
    pub price: Decimal,
    /// Trigger for SL / SL-M orders.
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub trigger_price: Option<Decimal>,
    pub after_market_order: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amo_time: Option<AmoTime>,
    /// Bracket order target distance.
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub bo_profit_value: Option<Decimal>,
    /// Bracket order stop-loss distance.
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub bo_stop_loss_value: Option<Decimal>,
}

impl PlaceOrderRequest {
    fn base(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        product: ProductType,
        order_type: OrderType,
    ) -> Self {
        Self {
            correlation_id: None,
            transaction_type: side,
            exchange_segment: segment,
            product_type: product,
            order_type,
            validity: Validity::Day,
            security_id,
            quantity,
            disclosed_quantity: None,
            price: Decimal::ZERO,
            trigger_price: None,
            after_market_order: false,
            amo_time: None,
            bo_profit_value: None,
            bo_stop_loss_value: None,
        }
    }

    pub fn market(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        product: ProductType,
    ) -> Self {
        Self::base(
            side,
            segment,
            security_id,
            quantity,
            product,
            OrderType::Market,
        )
    }

    pub fn limit(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        price: Decimal,
        product: ProductType,
    ) -> Self {
        Self {
            price,
            ..Self::base(
                side,
                segment,
                security_id,
                quantity,
                product,
                OrderType::Limit,
            )
        }
    }

    /// Stop-loss limit: becomes a limit order at `price` once `trigger` trades.
    pub fn stop_loss(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        price: Decimal,
        trigger: Decimal,
        product: ProductType,
    ) -> Self {
        Self {
            price,
            trigger_price: Some(trigger),
            ..Self::base(
                side,
                segment,
                security_id,
                quantity,
                product,
                OrderType::StopLoss,
            )
        }
    }

    /// Stop-loss market: becomes a market order once `trigger` trades.
    pub fn stop_loss_market(
        side: TransactionType,
        segment: ExchangeSegment,
        security_id: SecurityId,
        quantity: u32,
        trigger: Decimal,
        product: ProductType,
    ) -> Self {
        Self {
            trigger_price: Some(trigger),
            ..Self::base(
                side,
                segment,
                security_id,
                quantity,
                product,
                OrderType::StopLossMarket,
            )
        }
    }

    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    pub fn with_validity(mut self, validity: Validity) -> Self {
        self.validity = validity;
        self
    }

    pub fn with_disclosed_quantity(mut self, qty: u32) -> Self {
        self.disclosed_quantity = Some(qty);
        self
    }

    pub fn after_market(mut self, when: AmoTime) -> Self {
        self.after_market_order = true;
        self.amo_time = Some(when);
        self
    }

    pub fn with_bracket(mut self, profit_value: Decimal, stop_loss_value: Decimal) -> Self {
        self.bo_profit_value = Some(profit_value);
        self.bo_stop_loss_value = Some(stop_loss_value);
        self
    }

    /// Local sanity checks so obviously bad orders never reach the exchange.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(DhanError::InvalidRequest(m.to_owned()));
        if let Some(id) = &self.correlation_id {
            validate_correlation_id(id)?;
        }
        if self.quantity == 0 {
            return bad("quantity must be > 0");
        }
        if matches!(self.product_type, ProductType::Unknown) {
            return bad("product type is Unknown");
        }
        if matches!(self.validity, Validity::Unknown) {
            return bad("validity is Unknown");
        }
        if self.price < Decimal::ZERO || self.trigger_price.is_some_and(|t| t <= Decimal::ZERO) {
            return bad("prices must be positive");
        }
        match self.order_type {
            OrderType::Limit if self.price <= Decimal::ZERO => {
                return bad("LIMIT order needs a price");
            }
            OrderType::StopLoss if self.price <= Decimal::ZERO || self.trigger_price.is_none() => {
                return bad("STOP_LOSS order needs a price and a trigger price");
            }
            OrderType::StopLossMarket if self.trigger_price.is_none() => {
                return bad("STOP_LOSS_MARKET order needs a trigger price");
            }
            OrderType::Market if self.price != Decimal::ZERO => {
                return bad("MARKET order must not carry a price");
            }
            OrderType::Unknown => return bad("order type is Unknown"),
            _ => {}
        }
        if let Some(dq) = self.disclosed_quantity
            && (dq > self.quantity || u64::from(dq) * 100 < u64::from(self.quantity) * 30)
        {
            return bad("disclosed quantity must be between 30% and 100% of quantity");
        }
        if self.after_market_order != self.amo_time.is_some() {
            return bad("after-market orders need amo_time, and only they may have it");
        }
        if matches!(self.product_type, ProductType::Bo)
            && (self.bo_profit_value.is_none() || self.bo_stop_loss_value.is_none())
        {
            return bad("bracket orders need bo_profit_value and bo_stop_loss_value");
        }
        Ok(())
    }
}

/// Changes to a pending order. Dhan expects the full order quantity (not the
/// remaining quantity) when changing quantity.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifyOrderRequest {
    pub order_id: String,
    pub order_type: OrderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leg_name: Option<LegName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<u32>,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub price: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosed_quantity: Option<u32>,
    #[serde(
        serialize_with = "su::ser_opt_decimal",
        skip_serializing_if = "Option::is_none"
    )]
    pub trigger_price: Option<Decimal>,
    pub validity: Validity,
}

impl ModifyOrderRequest {
    pub fn new(order_id: impl Into<String>, order_type: OrderType) -> Self {
        Self {
            order_id: order_id.into(),
            order_type,
            leg_name: None,
            quantity: None,
            price: None,
            disclosed_quantity: None,
            trigger_price: None,
            validity: Validity::Day,
        }
    }

    pub fn with_price(mut self, price: Decimal) -> Self {
        self.price = Some(price);
        self
    }

    pub fn with_quantity(mut self, quantity: u32) -> Self {
        self.quantity = Some(quantity);
        self
    }

    pub fn with_trigger_price(mut self, trigger: Decimal) -> Self {
        self.trigger_price = Some(trigger);
        self
    }

    pub fn with_leg(mut self, leg: LegName) -> Self {
        self.leg_name = Some(leg);
        self
    }

    pub fn with_validity(mut self, validity: Validity) -> Self {
        self.validity = validity;
        self
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(DhanError::InvalidRequest(m.to_owned()));
        if self.order_id.trim().is_empty() {
            return bad("order id is empty");
        }
        if matches!(self.order_type, OrderType::Unknown)
            || matches!(self.validity, Validity::Unknown)
        {
            return bad("order type / validity is Unknown");
        }
        if self.quantity == Some(0) {
            return bad("quantity must be > 0");
        }
        if self.price.is_some_and(|p| p <= Decimal::ZERO)
            || self.trigger_price.is_some_and(|p| p <= Decimal::ZERO)
        {
            return bad("prices must be positive");
        }
        if self.quantity.is_none() && self.price.is_none() && self.trigger_price.is_none() {
            return bad("nothing to modify");
        }
        Ok(())
    }
}

/// Dhan's immediate answer to place / modify / cancel. The order's final
/// state comes later, via the order book or the order-update stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderAck {
    #[serde(deserialize_with = "su::string_lenient")]
    pub order_id: String,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub order_status: OrderStatus,
}

/// An entry in the order book.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    #[serde(deserialize_with = "su::string_lenient")]
    pub order_id: String,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub exchange_order_id: Option<String>,
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
    pub disclosed_quantity: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub price: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub trigger_price: Decimal,
    #[serde(default, deserialize_with = "su::bool_lenient")]
    pub after_market_order: bool,
    #[serde(default, deserialize_with = "su::decimal")]
    pub bo_profit_value: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub bo_stop_loss_value: Decimal,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub leg_name: Option<LegName>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub create_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub update_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub exchange_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub oms_error_code: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub oms_error_description: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub algo_id: Option<String>,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub average_traded_price: Decimal,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub filled_qty: i64,
}

/// One execution (fill).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trade {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub dhan_client_id: String,
    #[serde(deserialize_with = "su::string_lenient")]
    pub order_id: String,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub exchange_order_id: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub exchange_trade_id: Option<String>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub transaction_type: Option<TransactionType>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub exchange_segment: Option<ExchangeSegment>,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub product_type: ProductType,
    #[serde(default, deserialize_with = "su::enum_lenient")]
    pub order_type: OrderType,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub trading_symbol: Option<String>,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub security_id: Option<SecurityId>,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub traded_quantity: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub traded_price: Decimal,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub create_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub update_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub exchange_time: Option<NaiveDateTime>,
}

impl DhanClient {
    /// Places an order. Never retried automatically; if the result is an
    /// error with [`DhanError::outcome_unknown`], look the order up with
    /// [`get_order_by_correlation_id`](Self::get_order_by_correlation_id)
    /// before trying again.
    pub async fn place_order(&self, req: &PlaceOrderRequest) -> Result<OrderAck> {
        req.validate()?;
        tracing::info!(
            security_id = %req.security_id, side = ?req.transaction_type, qty = req.quantity,
            order_type = ?req.order_type, correlation_id = ?req.correlation_id, "placing order"
        );
        self.send_json(Method::POST, "/orders", req, true, ApiCategory::Order)
            .await
    }

    /// Places an order that Dhan splits into several to stay under the
    /// exchange freeze quantity (mainly for F&O).
    pub async fn place_slice_order(&self, req: &PlaceOrderRequest) -> Result<Vec<OrderAck>> {
        req.validate()?;
        let v: serde_json::Value = self
            .send_json(
                Method::POST,
                "/orders/slicing",
                req,
                true,
                ApiCategory::Order,
            )
            .await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/orders/slicing", e, b""))
    }

    pub async fn modify_order(&self, req: &ModifyOrderRequest) -> Result<OrderAck> {
        req.validate()?;
        let path = format!("/orders/{}", req.order_id);
        self.send_json(Method::PUT, &path, req, true, ApiCategory::Order)
            .await
    }

    /// Cancels a pending order. Returns `None` when Dhan answers `202` with
    /// no body; the order-update stream will confirm the cancellation.
    pub async fn cancel_order(&self, order_id: &str) -> Result<Option<OrderAck>> {
        if order_id.trim().is_empty() {
            return Err(DhanError::InvalidRequest("order id is empty".into()));
        }
        let path = format!("/orders/{order_id}");
        self.send_json_opt::<(), _>(Method::DELETE, &path, None, &[], false, ApiCategory::Order)
            .await
    }

    /// All orders for the day.
    pub async fn order_book(&self) -> Result<Vec<Order>> {
        let v = self.get_value("/orders", ApiCategory::NonTrading).await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/orders", e, b""))
    }

    pub async fn get_order(&self, order_id: &str) -> Result<Order> {
        self.get(&format!("/orders/{order_id}"), ApiCategory::NonTrading)
            .await
    }

    /// Looks up an order by the correlation ID set when placing it. This is
    /// how to recover after a timeout on [`place_order`](Self::place_order).
    pub async fn get_order_by_correlation_id(&self, correlation_id: &str) -> Result<Order> {
        validate_correlation_id(correlation_id)?;
        self.get(
            &format!("/orders/external/{correlation_id}"),
            ApiCategory::NonTrading,
        )
        .await
    }

    /// All fills for the day.
    pub async fn trade_book(&self) -> Result<Vec<Trade>> {
        let v = self.get_value("/trades", ApiCategory::NonTrading).await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/trades", e, b""))
    }

    /// All fills of one order (several for partial executions).
    pub async fn trades_for_order(&self, order_id: &str) -> Result<Vec<Trade>> {
        let path = format!("/trades/{order_id}");
        let v = self.get_value(&path, ApiCategory::NonTrading).await?;
        su::one_or_many(v).map_err(|e| DhanError::decode(&path, e, b""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buy_limit() -> PlaceOrderRequest {
        PlaceOrderRequest::limit(
            TransactionType::Buy,
            ExchangeSegment::NseEq,
            SecurityId(11536),
            5,
            Decimal::new(334_580, 2),
            ProductType::Intraday,
        )
    }

    #[test]
    fn validation_catches_bad_orders() {
        assert!(buy_limit().validate().is_ok());

        let mut zero_qty = buy_limit();
        zero_qty.quantity = 0;
        assert!(zero_qty.validate().is_err());

        let mut no_price = buy_limit();
        no_price.price = Decimal::ZERO;
        assert!(no_price.validate().is_err());

        let sl_m = PlaceOrderRequest::stop_loss_market(
            TransactionType::Sell,
            ExchangeSegment::NseEq,
            SecurityId(11536),
            5,
            Decimal::new(3300, 0),
            ProductType::Intraday,
        );
        assert!(sl_m.validate().is_ok());

        assert!(
            buy_limit()
                .with_correlation_id("bad/id")
                .validate()
                .is_err()
        );
        assert!(
            buy_limit()
                .with_correlation_id("x".repeat(31))
                .validate()
                .is_err()
        );
        assert!(buy_limit().with_disclosed_quantity(1).validate().is_err());
    }

    #[test]
    fn serializes_as_dhan_expects() -> Result<(), serde_json::Error> {
        let v = serde_json::to_value(buy_limit().with_correlation_id("abc-1"))?;
        assert_eq!(v["transactionType"], "BUY");
        assert_eq!(v["exchangeSegment"], "NSE_EQ");
        assert_eq!(v["productType"], "INTRADAY");
        assert_eq!(v["orderType"], "LIMIT");
        assert_eq!(v["securityId"], "11536");
        assert_eq!(v["quantity"], 5);
        assert_eq!(v["price"], 3345.8);
        assert_eq!(v["correlationId"], "abc-1");
        assert!(v.get("triggerPrice").is_none());
        Ok(())
    }
}
