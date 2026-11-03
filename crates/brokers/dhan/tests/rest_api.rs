//! REST client against a local mock of Dhan's API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use dhan::*;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client(server: &MockServer) -> DhanClient {
    let cfg = DhanConfig::new("1000000003", Secret::new("test-token"))
        .with_api_base(server.uri())
        .with_request_timeout(Duration::from_secs(2));
    DhanClient::new(cfg).unwrap()
}

fn d(units: i64, scale: u32) -> Decimal {
    Decimal::new(units, scale)
}

#[tokio::test]
async fn place_order_sends_expected_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("access-token", "test-token"))
        .and(header("client-id", "1000000003"))
        .and(body_partial_json(json!({
            "dhanClientId": "1000000003",
            "correlationId": "orb-1",
            "transactionType": "BUY",
            "exchangeSegment": "NSE_EQ",
            "productType": "INTRADAY",
            "orderType": "LIMIT",
            "validity": "DAY",
            "securityId": "11536",
            "quantity": 5,
            "price": 3345.8,
            "afterMarketOrder": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "orderId": "112111182198", "orderStatus": "PENDING"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let req = PlaceOrderRequest::limit(
        TransactionType::Buy,
        ExchangeSegment::NseEq,
        SecurityId(11536),
        5,
        d(334_580, 2),
        ProductType::Intraday,
    )
    .with_correlation_id("orb-1");
    let ack = client(&server).await.place_order(&req).await.unwrap();
    assert_eq!(ack.order_id, "112111182198");
    assert_eq!(ack.order_status, OrderStatus::Pending);
}

#[tokio::test]
async fn invalid_order_never_reaches_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let bad = PlaceOrderRequest::limit(
        TransactionType::Buy,
        ExchangeSegment::NseEq,
        SecurityId(11536),
        0,
        d(100, 0),
        ProductType::Intraday,
    );
    let err = client(&server).await.place_order(&bad).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Input);
}

#[tokio::test]
async fn order_placement_is_not_retried_on_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "errorType": "Internal_Server_Error", "errorCode": "DH-908", "errorMessage": "boom"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let req = PlaceOrderRequest::market(
        TransactionType::Sell,
        ExchangeSegment::NseEq,
        SecurityId(1333),
        1,
        ProductType::Intraday,
    );
    let err = client(&server).await.place_order(&req).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Server);
    // Dhan saw the request, so the order may exist: caller must reconcile.
    assert!(err.outcome_unknown());
}

#[tokio::test]
async fn order_rejection_is_reported_with_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errorType": "Order_Error", "errorCode": "DH-906", "errorMessage": "Insufficient funds"
        })))
        .mount(&server)
        .await;

    let req = PlaceOrderRequest::market(
        TransactionType::Buy,
        ExchangeSegment::NseEq,
        SecurityId(1333),
        1,
        ProductType::Intraday,
    );
    match client(&server).await.place_order(&req).await.unwrap_err() {
        DhanError::Api {
            kind,
            code,
            message,
            ..
        } => {
            assert_eq!(kind, ErrorKind::Order);
            assert_eq!(code.as_deref(), Some("DH-906"));
            assert_eq!(message, "Insufficient funds");
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[tokio::test]
async fn reads_are_retried_on_transient_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/positions"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/positions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "dhanClientId": "1000000009", "tradingSymbol": "TCS", "securityId": "11536",
            "positionType": "LONG", "exchangeSegment": "NSE_EQ", "productType": "INTRADAY",
            "buyAvg": 3345.8, "buyQty": 40, "costPrice": 3215.0, "sellAvg": 0.0, "sellQty": 0,
            "netQty": 40, "realizedProfit": 0.0, "unrealizedProfit": 6122.0,
            "drvExpiryDate": "0001-01-01", "drvOptionType": null, "crossCurrency": false
        }])))
        .expect(1)
        .mount(&server)
        .await;

    let positions = client(&server).await.positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    let p = &positions[0];
    assert_eq!(p.net_qty, 40);
    assert!(p.is_open());
    assert_eq!(p.position_type, PositionType::Long);
    assert_eq!(p.product_type, ProductType::Intraday);
    assert_eq!(p.unrealized_profit, d(6122, 0));
}

#[tokio::test]
async fn order_book_tolerates_messy_fields() {
    let server = MockServer::start().await;
    // Taken from Dhan's docs, plus the kinds of irregularities seen in
    // practice: strings for numbers, blanks, nulls and unknown enums.
    Mock::given(method("GET"))
        .and(path("/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "dhanClientId": "1000000003", "orderId": "112111182198", "correlationId": "123abc678",
                "orderStatus": "PART_TRADED", "transactionType": "BUY", "exchangeSegment": "NSE_EQ",
                "productType": "INTRADAY", "orderType": "LIMIT", "validity": "DAY", "tradingSymbol": "",
                "securityId": "11536", "quantity": "5", "disclosedQuantity": 0, "price": "3345.80",
                "triggerPrice": 0.0, "afterMarketOrder": false, "legName": "",
                "createTime": "2021-11-24 13:33:03", "updateTime": "2021-11-24 13:33:03",
                "exchangeTime": "NA", "drvExpiryDate": null, "omsErrorCode": null,
                "remainingQuantity": 3, "averageTradedPrice": 3345.8, "filledQty": 2
            },
            {
                "orderId": 112_111_182_199_i64, "orderStatus": "BRAND_NEW_STATUS", "productType": "???",
                "securityId": 1333, "quantity": 1
            }
        ])))
        .mount(&server)
        .await;

    let book = client(&server).await.order_book().await.unwrap();
    assert_eq!(book.len(), 2);
    let a = &book[0];
    assert_eq!(a.order_status, OrderStatus::PartTraded);
    assert_eq!(a.quantity, 5);
    assert_eq!(a.price, d(334_580, 2));
    assert_eq!(a.filled_qty, 2);
    assert_eq!(a.trading_symbol, None);
    assert_eq!(a.leg_name, None);
    assert!(a.create_time.is_some());
    assert!(a.exchange_time.is_none());
    let b = &book[1];
    assert_eq!(b.order_id, "112111182199");
    assert_eq!(b.order_status, OrderStatus::Unknown);
    assert_eq!(b.product_type, ProductType::Unknown);
    assert_eq!(b.security_id, Some(SecurityId(1333)));
}

#[tokio::test]
async fn cancel_accepts_empty_202() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/orders/112111182045"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    let ack = client(&server)
        .await
        .cancel_order("112111182045")
        .await
        .unwrap();
    assert!(ack.is_none());
}

#[tokio::test]
async fn super_order_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/super/orders"))
        .and(body_partial_json(json!({
            "dhanClientId": "1000000003", "transactionType": "BUY", "orderType": "LIMIT",
            "price": 1500.0, "targetPrice": 1600.0, "stopLossPrice": 1400.0, "trailingJump": 10.0
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "orderId": "5925022734212", "orderStatus": "PENDING"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/super/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "dhanClientId": "1100003626", "orderId": "5925022734212", "orderStatus": "PENDING",
            "transactionType": "BUY", "exchangeSegment": "NSE_EQ", "productType": "INTRADAY",
            "orderType": "LIMIT", "validity": "DAY", "tradingSymbol": "HDFCBANK", "securityId": "1333",
            "quantity": 10, "remainingQuantity": 10, "ltp": 1660.95, "price": 1500,
            "legName": "ENTRY_LEG", "legDetails": [
                {"orderId": "5925022734212", "legName": "STOP_LOSS_LEG", "transactionType": "SELL",
                 "totalQuatity": 0, "remainingQuantity": 0, "triggeredQuantity": 0, "price": 1400,
                 "orderStatus": "PENDING", "trailingJump": 10},
                {"orderId": "5925022734212", "legName": "TARGET_LEG", "transactionType": "SELL",
                 "remainingQuantity": 0, "triggeredQuantity": 0, "price": 1550,
                 "orderStatus": "PENDING", "trailingJump": 0}
            ]
        }])))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let req = PlaceSuperOrderRequest::limit(
        TransactionType::Buy,
        ExchangeSegment::NseEq,
        SecurityId(1333),
        10,
        d(1500, 0),
        d(1600, 0),
        d(1400, 0),
        ProductType::Intraday,
    )
    .with_trailing_jump(d(10, 0));
    let ack = c.place_super_order(&req).await.unwrap();
    assert_eq!(ack.order_id, "5925022734212");

    let book = c.super_order_book().await.unwrap();
    let legs = &book[0].leg_details;
    assert_eq!(legs.len(), 2);
    assert_eq!(legs[0].leg_name, Some(LegName::StopLossLeg));
    assert_eq!(legs[0].trailing_jump, d(10, 0));
    assert_eq!(legs[1].price, d(1550, 0));
}

#[tokio::test]
async fn quote_snapshot_is_flattened() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/marketfeed/ohlc"))
        .and(header("client-id", "1000000003"))
        .and(body_partial_json(json!({"NSE_EQ": [11536]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"NSE_EQ": {"11536": {"last_price": 4525.55,
                "ohlc": {"open": 4521.45, "close": 4507.85, "high": 4530, "low": 4500}}}},
            "status": "success"
        })))
        .mount(&server)
        .await;

    let id = InstrumentId::nse_eq(11536);
    let quotes = client(&server).await.ohlc(&[id]).await.unwrap();
    let q = &quotes[&id];
    assert_eq!(q.last_price, d(452_555, 2));
    assert_eq!(q.ohlc.high, d(4530, 0));
}

#[tokio::test]
async fn intraday_candles_are_zipped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/charts/intraday"))
        .and(body_partial_json(json!({
            "securityId": "1333", "exchangeSegment": "NSE_EQ", "instrument": "EQUITY",
            "interval": 1, "fromDate": "2024-09-11 09:30:00", "toDate": "2024-09-15 13:00:00"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "open": [3750, 3757.85], "high": [3750, 3757.9], "low": [3750, 3746.1],
            "close": [3750, 3751.25], "volume": [166, 53629],
            "timestamp": [1328845020, 1328845080]
        })))
        .mount(&server)
        .await;

    let from = chrono::NaiveDate::from_ymd_opt(2024, 9, 11)
        .unwrap()
        .and_hms_opt(9, 30, 0)
        .unwrap();
    let to = chrono::NaiveDate::from_ymd_opt(2024, 9, 15)
        .unwrap()
        .and_hms_opt(13, 0, 0)
        .unwrap();
    let req = IntradayCandlesRequest::equity(
        ExchangeSegment::NseEq,
        SecurityId(1333),
        CandleInterval::Min1,
        from,
        to,
    );
    let candles = client(&server).await.intraday_candles(&req).await.unwrap();
    assert_eq!(candles.len(), 2);
    assert_eq!(candles[1].close, d(375_125, 2));
    assert_eq!(candles[1].volume, 53629);
    assert!(candles[0].time_ist().is_some());
}

#[tokio::test]
async fn intraday_range_rejects_over_90_days_per_call() {
    let server = MockServer::start().await;
    let from = chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(9, 15, 0)
        .unwrap();
    let req = IntradayCandlesRequest::equity(
        ExchangeSegment::NseEq,
        SecurityId(1333),
        CandleInterval::Min5,
        from,
        from + chrono::TimeDelta::days(120),
    );
    let err = client(&server)
        .await
        .intraday_candles(&req)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Input);
}

#[tokio::test]
async fn fund_limit_handles_dhan_typos() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/fundlimit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dhanClientId": "1000000009", "availabelBalance": 98440.0, "sodLimit": 113642,
            "collateralAmount": 0.0, "receiveableAmount": 0.0, "utilizedAmount": 15202.0,
            "blockedPayoutAmount": 0.0, "withdrawableBalance": 98310.0
        })))
        .mount(&server)
        .await;
    let f = client(&server).await.fund_limit().await.unwrap();
    assert_eq!(f.available_balance, d(98440, 0));
    assert_eq!(f.utilized_amount, d(15202, 0));
}

#[tokio::test]
async fn kill_switch_uses_query_parameter() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/killswitch"))
        .and(query_param("killSwitchStatus", "ACTIVATE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dhanClientId": "1000000009",
            "killSwitchStatus": "Kill Switch has been successfully activated"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let s = client(&server).await.set_kill_switch(true).await.unwrap();
    assert!(s.is_active());
}

#[tokio::test]
async fn expired_token_is_classified_as_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/profile"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errorType": "Invalid_Authentication", "errorCode": "DH-901",
            "errorMessage": "Client ID or user generated access token is invalid or expired."
        })))
        .expect(1) // auth errors are not retried
        .mount(&server)
        .await;
    let err = client(&server).await.profile().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Auth);
    assert!(!err.is_retryable());
}

#[tokio::test]
async fn token_can_be_swapped_at_runtime() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/profile"))
        .and(header("access-token", "new-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dhanClientId": "1000000003", "tokenValidity": "30/03/2025 15:37",
            "activeSegment": "Equity", "ddpi": "Active", "mtf": "Active",
            "dataPlan": "Active", "dataValidity": "2024-12-05 09:37:52.0"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server).await;
    c.set_access_token(Secret::new("new-token"));
    let p = c.profile().await.unwrap();
    assert!(p.has_data_plan());
}

#[tokio::test]
async fn local_rate_limit_fails_fast() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(2)
        .mount(&server)
        .await;
    let cfg = DhanConfig::new("1000000003", Secret::new("t"))
        .with_api_base(server.uri())
        .with_rate_limits(RateLimitConfig {
            non_trading: vec![Limit::per_hour(2)],
            max_wait: Duration::from_millis(10),
            ..RateLimitConfig::default()
        });
    let c = DhanClient::new(cfg).unwrap();
    c.order_book().await.unwrap();
    c.order_book().await.unwrap();
    let err = c.order_book().await.unwrap_err();
    assert!(matches!(err, DhanError::RateLimitedLocally { .. }));
}

#[tokio::test]
async fn secrets_never_appear_in_debug_output() {
    let server = MockServer::start().await;
    let c = client(&server).await;
    let dump = format!("{c:?}");
    assert!(!dump.contains("test-token"));
}
