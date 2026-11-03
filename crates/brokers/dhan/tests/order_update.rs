//! Order-update messages as documented by Dhan.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use dhan::order_update::{OrderUpdateEvent, parse_message};
use dhan::{Decimal, OrderStatus, OrderType, ProductType, SecurityId, TransactionType};

/// Dhan's documented sample, which repeats `Remarks` and mixes key casing.
const DOC_SAMPLE: &str = r#"{
    "Data": {
        "series": "EQ", "goodTillDaysDate": "2024-09-11", "instrumentType": "EQ",
        "refLtp": 13.21, "tickSize": 0.01, "algoId": "0", "multiplier": 1,
        "Exchange": "NSE", "Segment": "E", "Source": "N", "SecurityId": "14366",
        "ClientId": "1000000001", "ExchOrderNo": "1400000000404591", "OrderNo": "1124091136546",
        "Product": "C", "TxnType": "B", "OrderType": "LMT", "Validity": "DAY",
        "DiscQuantity": 1, "DiscQtyRem": 1, "RemainingQuantity": 1, "Quantity": 1,
        "TradedQty": 0, "Price": 13, "TriggerPrice": 0, "TradedPrice": 0, "AvgTradedPrice": 0,
        "AlgoOrdNo": "", "OffMktFlag": "0", "OrderDateTime": "2024-09-11 14:39:29",
        "ExchOrderTime": "2024-09-11 14:39:29", "LastUpdatedTime": "2024-09-11 14:39:29",
        "Remarks": "NR", "MktType": "NL", "ReasonDescription": "CONFIRMED", "LegNo": 1,
        "Instrument": "EQUITY", "Symbol": "IDEA", "ProductName": "CNC", "Status": "Cancelled",
        "LotSize": 1, "StrikePrice": "", "ExpiryDate": "0001-01-01 00:00:00", "OptType": "XX",
        "DisplayName": "Vodafone Idea", "Isin": "INE669E01016", "Series": "EQ",
        "GoodTillDaysDate": "2024-09-11", "RefLtp": 13.21, "TickSize": 0.01, "AlgoId": "0",
        "Multiplier": 1, "CorrelationId": "", "Remarks": "Super Order"
    },
    "Type": "order_alert"
}"#;

#[test]
fn parses_documented_sample() {
    let OrderUpdateEvent::Order(u) = parse_message(DOC_SAMPLE) else {
        panic!("expected an order update");
    };
    assert_eq!(u.order_no, "1124091136546");
    assert_eq!(u.security_id, Some(SecurityId(14366)));
    assert_eq!(u.order_status(), OrderStatus::Cancelled);
    assert_eq!(u.side(), Some(TransactionType::Buy));
    assert_eq!(u.product_type(), ProductType::Cnc);
    assert_eq!(u.order_kind(), OrderType::Limit);
    assert_eq!(u.price, Decimal::new(13, 0));
    assert_eq!(u.ref_ltp, Some(Decimal::new(1321, 2)));
    assert_eq!(u.correlation_id, None);
    // The later duplicate key wins.
    assert!(u.is_super_order_leg());
    assert!(u.order_date_time.is_some());
}

#[test]
fn intraday_fill_update() {
    let msg = r#"{"Type":"order_alert","Data":{"OrderNo":"9","SecurityId":"2885","Product":"I",
        "TxnType":"S","OrderType":"SLM","Status":"TRADED","Quantity":"10","TradedQty":10,
        "TradedPrice":"1428.05","AvgTradedPrice":1428.05,"CorrelationId":"orb-2885-1"}}"#;
    let OrderUpdateEvent::Order(u) = parse_message(msg) else {
        panic!()
    };
    assert_eq!(u.order_status(), OrderStatus::Traded);
    assert_eq!(u.product_type(), ProductType::Intraday);
    assert_eq!(u.order_kind(), OrderType::StopLossMarket);
    assert_eq!(u.side(), Some(TransactionType::Sell));
    assert_eq!(u.quantity, 10);
    assert_eq!(u.avg_traded_price, Decimal::new(142_805, 2));
    assert_eq!(u.correlation_id.as_deref(), Some("orb-2885-1"));
}

#[test]
fn other_and_bad_messages() {
    assert!(matches!(
        parse_message(r#"{"Type":"login","status":"ok"}"#),
        OrderUpdateEvent::Other(_)
    ));
    assert!(matches!(
        parse_message("not json"),
        OrderUpdateEvent::ParseError(_)
    ));
    assert!(matches!(
        parse_message(r#"{"Type":"order_alert"}"#),
        OrderUpdateEvent::ParseError(_)
    ));
}
