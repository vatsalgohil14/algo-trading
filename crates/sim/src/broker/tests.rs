use trading_core::{Bracket, Product, StrategyId};

use super::*;

const TICK: Price = Price::from_paise(5);

fn p(rupees: f64) -> Price {
    Price::from_rupees_f64(rupees).unwrap()
}

fn t(minute: i64) -> Timestamp {
    Timestamp::from_secs(1_736_400_000).plus_minutes(minute)
}

fn id(s: &str) -> ClientOrderId {
    ClientOrderId::new(s).unwrap()
}

fn bar(minute: i64, ohlc: [f64; 4], volume: u64) -> Bar {
    Bar {
        instrument: InstrumentId(0),
        ts: t(minute),
        open: p(ohlc[0]),
        high: p(ohlc[1]),
        low: p(ohlc[2]),
        close: p(ohlc[3]),
        volume,
    }
}

fn order(name: &str, side: Side, kind: OrderKind, bracket: Option<Bracket>) -> OrderRequest {
    OrderRequest {
        client_id: id(name),
        strategy: StrategyId(0),
        instrument: InstrumentId(0),
        side,
        qty: 100,
        kind,
        product: Product::Intraday,
        bracket,
        created: t(0),
    }
}

fn long_entry(limit: f64) -> OrderRequest {
    order(
        "e",
        Side::Buy,
        OrderKind::Limit { price: p(limit) },
        Some(Bracket {
            stop: p(495.0),
            target: Some(p(510.0)),
        }),
    )
}

fn broker(config: SimBrokerConfig) -> SimBroker {
    SimBroker::new(config, vec![TICK])
}

fn fills(b: &mut SimBroker) -> Vec<(Side, u32, Price)> {
    b.take_reports()
        .into_iter()
        .filter_map(|r| match r.event {
            OrderEvent::Fill(f) => Some((f.side, f.qty, f.price)),
            _ => None,
        })
        .collect()
}

fn entered(b: &mut SimBroker) {
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [500.0, 500.5, 499.0, 500.0], 100_000));
    assert_eq!(fills(b).len(), 1);
}

#[test]
fn no_fill_on_a_bar_before_the_order_existed() {
    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(1)).unwrap();
    b.on_bar(&bar(0, [500.0, 500.5, 499.0, 500.0], 100_000));
    assert!(fills(&mut b).is_empty());
}

#[test]
fn marketable_limit_pays_slippage_up_to_the_limit() {
    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [499.5, 500.5, 499.0, 500.0], 100_000));
    // Open 499.50 plus 10 paise of slippage.
    assert_eq!(fills(&mut b), [(Side::Buy, 100, p(499.6))]);

    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [500.0, 500.5, 499.0, 500.0], 100_000));
    assert_eq!(fills(&mut b), [(Side::Buy, 100, p(500.0))]);
}

#[test]
fn limit_must_be_traded_through() {
    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(0)).unwrap();
    // Opens above and only touches the limit: the order is missed.
    b.on_bar(&bar(0, [501.0, 502.0, 500.0, 501.5], 100_000));
    assert!(fills(&mut b).is_empty());
    // Runs away entirely.
    b.on_bar(&bar(1, [503.0, 505.0, 502.5, 504.0], 100_000));
    assert!(fills(&mut b).is_empty());
    // Trades through it.
    b.on_bar(&bar(2, [501.0, 502.0, 499.9, 501.5], 100_000));
    assert_eq!(fills(&mut b), [(Side::Buy, 100, p(500.0))]);
}

#[test]
fn fills_are_limited_by_volume() {
    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [499.0, 499.5, 498.0, 499.0], 400)); // 10% of 400
    assert_eq!(fills(&mut b).first().map(|f| f.1), Some(40));
    b.on_bar(&bar(1, [499.0, 499.5, 498.0, 499.0], 0));
    assert!(fills(&mut b).is_empty());
    b.on_bar(&bar(2, [499.0, 499.5, 498.0, 499.0], 100_000));
    assert_eq!(fills(&mut b).first().map(|f| f.1), Some(60));
}

#[test]
fn stop_fills_with_slippage_and_at_a_gap() {
    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    b.on_bar(&bar(1, [497.0, 497.5, 494.5, 495.0], 1000));
    assert_eq!(fills(&mut b), [(Side::Sell, 100, p(494.9))]);
    assert!(b.positions().is_empty());

    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    b.on_bar(&bar(1, [490.0, 491.0, 489.0, 490.0], 1000));
    assert_eq!(fills(&mut b), [(Side::Sell, 100, p(489.9))]);
}

#[test]
fn target_needs_a_trade_beyond_it() {
    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    b.on_bar(&bar(1, [508.0, 510.0, 507.0, 509.0], 1000));
    assert!(fills(&mut b).is_empty());
    b.on_bar(&bar(2, [509.0, 510.5, 508.0, 510.0], 1000));
    assert_eq!(fills(&mut b), [(Side::Sell, 100, p(510.0))]);
}

#[test]
fn stop_comes_first_when_a_bar_reaches_both() {
    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    b.on_bar(&bar(1, [500.0, 512.0, 494.0, 505.0], 1000));
    let f = fills(&mut b);
    assert_eq!(f.len(), 1);
    assert!(f[0].2 < p(495.0));
}

#[test]
fn on_the_entry_bar_the_stop_is_live_but_the_target_is_not() {
    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [499.0, 511.0, 498.0, 505.0], 100_000));
    assert_eq!(fills(&mut b).len(), 1);

    let mut b = broker(SimBrokerConfig::default());
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [499.0, 500.0, 494.0, 496.0], 100_000));
    assert_eq!(fills(&mut b).len(), 2);
}

#[test]
fn short_side_is_the_mirror_image() {
    let mut b = broker(SimBrokerConfig::default());
    let short = order(
        "s",
        Side::Sell,
        OrderKind::Limit { price: p(500.0) },
        Some(Bracket {
            stop: p(505.0),
            target: Some(p(490.0)),
        }),
    );
    b.submit(&short, t(0)).unwrap();
    b.on_bar(&bar(0, [500.5, 501.0, 499.5, 500.0], 100_000));
    assert_eq!(fills(&mut b), [(Side::Sell, 100, p(500.35))]);
    assert_eq!(b.positions()[0].qty, -100);
    b.on_bar(&bar(1, [492.0, 492.0, 489.0, 490.0], 1000));
    assert_eq!(fills(&mut b), [(Side::Buy, 100, p(490.0))]);
}

#[test]
fn market_exit_gets_out_regardless_of_volume() {
    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    b.cancel(id("e"), t(1)).unwrap();
    let exit = order("x", Side::Sell, OrderKind::Market, None);
    b.submit(&exit, t(1)).unwrap();
    b.take_reports();
    b.on_bar(&bar(1, [502.0, 502.5, 494.0, 502.0], 1));
    // The cancelled stop no longer fires; the exit fills at the open.
    assert_eq!(fills(&mut b), [(Side::Sell, 100, p(501.85))]);
    assert!(b.positions().is_empty());
}

#[test]
fn stop_can_be_moved_and_cancels_are_checked() {
    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    let change = OrderChange {
        trigger: Some(p(500.0)),
        ..OrderChange::default()
    };
    b.modify(id("e"), change, t(1)).unwrap();
    b.on_bar(&bar(1, [501.0, 501.5, 499.5, 500.0], 1000));
    assert_eq!(fills(&mut b), [(Side::Sell, 100, p(499.9))]);
    assert!(matches!(
        b.cancel(id("e"), t(2)),
        Err(ExecError::Rejected(_))
    ));
    assert!(matches!(
        b.cancel(id("nope"), t(2)),
        Err(ExecError::UnknownOrder(_))
    ));
}

#[test]
fn injected_faults() {
    let with = |faults: Faults| {
        broker(SimBrokerConfig {
            faults,
            ..SimBrokerConfig::default()
        })
    };
    let mut b = with(Faults {
        reject: 1.0,
        ..Faults::default()
    });
    assert!(matches!(
        b.submit(&long_entry(500.0), t(0)),
        Err(ExecError::Rejected(_))
    ));

    let mut b = with(Faults {
        unknown_not_placed: 1.0,
        ..Faults::default()
    });
    assert!(matches!(
        b.submit(&long_entry(500.0), t(0)),
        Err(ExecError::OutcomeUnknown(_))
    ));
    assert_eq!(b.order_status(id("e")), None);

    let mut b = with(Faults {
        unknown_placed: 1.0,
        ..Faults::default()
    });
    assert!(matches!(
        b.submit(&long_entry(500.0), t(0)),
        Err(ExecError::OutcomeUnknown(_))
    ));
    assert_eq!(b.order_status(id("e")).unwrap().status, OrderStatus::Open);

    let mut b = with(Faults {
        duplicate_fill: 1.0,
        ..Faults::default()
    });
    b.submit(&long_entry(500.0), t(0)).unwrap();
    b.on_bar(&bar(0, [500.0, 500.5, 499.0, 500.0], 100_000));
    assert_eq!(fills(&mut b).len(), 2);
    assert_eq!(b.positions()[0].qty, 100, "the broker's position is real");

    let mut b = with(Faults {
        cancel_fails: 1.0,
        ..Faults::default()
    });
    b.submit(&long_entry(500.0), t(0)).unwrap();
    assert!(b.cancel(id("e"), t(0)).is_err());
    assert_eq!(b.order_status(id("e")).unwrap().status, OrderStatus::Open);
}

#[test]
fn end_of_day_expires_orders_and_closes_positions() {
    let mut b = broker(SimBrokerConfig::default());
    entered(&mut b);
    b.end_of_day(t(400));
    let reports = b.take_reports();
    assert!(reports.iter().any(|r| r.event == OrderEvent::Expired));
    assert!(
        reports
            .iter()
            .any(|r| matches!(r.event, OrderEvent::Fill(_)))
    );
    assert!(b.positions().is_empty());
    assert_eq!(b.auto_square_offs(), 1);
}
