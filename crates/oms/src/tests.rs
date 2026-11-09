use trading_core::{Exchange, Universe};

use super::*;

fn p(rupees: i64) -> Price {
    Price::from_paise(rupees * 100)
}

fn now() -> Timestamp {
    Timestamp::from_secs(1_736_400_000)
}

struct H {
    oms: Oms,
    inst: Instrument,
    trade: u32,
}

impl H {
    fn new() -> Self {
        let mut u = Universe::new();
        let id = u
            .add(Exchange::Nse, "S", 1, Price::from_paise(5), 1)
            .unwrap();
        u.add(Exchange::Nse, "T", 2, Price::from_paise(5), 1)
            .unwrap();
        Self {
            oms: Oms::new(7, 2),
            inst: u.get(id).unwrap().clone(),
            trade: 0,
        }
    }

    fn id(&self) -> InstrumentId {
        self.inst.id
    }

    fn enter(&mut self) -> ClientOrderId {
        let order = EntryOrder {
            strategy: StrategyId(3),
            side: Side::Buy,
            qty: 60,
            limit: Some(p(500)),
            stop: p(495),
            target: Some(p(510)),
        };
        let id = self.oms.enter(&self.inst, &order, p(500), now()).unwrap();
        assert!(matches!(self.commands()[..], [Command::Submit(_)]));
        id
    }

    fn commands(&mut self) -> Vec<Command> {
        self.oms.take_commands()
    }

    fn report(&mut self, id: ClientOrderId, event: OrderEvent) {
        self.oms.on_report(
            &ExecutionReport {
                client_id: id,
                broker_order_id: None,
                ts: now(),
                event,
            },
            now(),
        );
    }

    fn fill(&mut self, id: ClientOrderId, side: Side, qty: u32, price: Price) {
        self.trade += 1;
        let f = self.make_fill(id, side, qty, price, &format!("t{}", self.trade));
        self.report(id, OrderEvent::Fill(f));
    }

    fn make_fill(&self, id: ClientOrderId, side: Side, qty: u32, price: Price, t: &str) -> Fill {
        Fill {
            client_id: id,
            instrument: self.inst.id,
            side,
            qty,
            price,
            ts: now(),
            trade_id: t.into(),
        }
    }

    fn entry_finished(&mut self) -> Vec<u32> {
        self.oms
            .take_events()
            .iter()
            .filter_map(|e| match e {
                OmsEvent::EntryFinished { filled, .. } => Some(*filled),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn entry_carries_its_stop_and_target() {
    let mut h = H::new();
    let order = EntryOrder {
        strategy: StrategyId(3),
        side: Side::Buy,
        qty: 60,
        limit: Some(p(500)),
        stop: p(495),
        target: Some(p(510)),
    };
    let id = h.oms.enter(&h.inst, &order, p(500), now()).unwrap();
    let cmds = h.commands();
    let Command::Submit(r) = &cmds[0] else {
        panic!("{cmds:?}")
    };
    assert_eq!(r.client_id, id);
    assert_eq!(r.product, Product::Intraday);
    assert_eq!(r.kind, OrderKind::Limit { price: p(500) });
    assert_eq!(
        r.bracket,
        Some(Bracket {
            stop: p(495),
            target: Some(p(510))
        })
    );
    assert_eq!(h.oms.owner(h.id()), Some(StrategyId(3)));
    assert!(matches!(
        h.oms.enter(&h.inst, &order, p(500), now()),
        Err(OmsError::Busy(_))
    ));
}

#[test]
fn invalid_entry_leaves_the_stock_free() {
    let mut h = H::new();
    let order = EntryOrder {
        strategy: StrategyId(3),
        side: Side::Buy,
        qty: 60,
        limit: Some(p(500)),
        stop: p(505), // wrong side
        target: None,
    };
    assert!(matches!(
        h.oms.enter(&h.inst, &order, p(500), now()),
        Err(OmsError::Invalid(_))
    ));
    assert!(!h.oms.is_busy(h.id()));
    assert!(h.commands().is_empty());
}

#[test]
fn fills_build_the_position_and_finish_the_entry() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 20, p(500));
    assert_eq!(h.entry_finished(), Vec::<u32>::new());
    h.fill(id, Side::Buy, 40, p(500));
    assert_eq!(h.entry_finished(), [60]);
    assert_eq!(
        h.oms.view(StrategyId(3), h.id()),
        (60, Some(p(500)), Money::ZERO)
    );
    // Another strategy sees nothing.
    assert_eq!(h.oms.view(StrategyId(4), h.id()).0, 0);

    // The stop leg closes the trade at the broker.
    h.fill(id, Side::Sell, 60, p(495));
    let (qty, avg, realized) = h.oms.view(StrategyId(3), h.id());
    assert_eq!((qty, avg), (0, None));
    assert_eq!(realized, Money::from_paise(-30_000));
    assert!(!h.oms.is_busy(h.id()));
    assert_eq!(h.oms.owner(h.id()), None);
}

#[test]
fn a_fill_reported_twice_counts_once() {
    let mut h = H::new();
    let id = h.enter();
    let f = h.make_fill(id, Side::Buy, 60, p(500), "same");
    h.report(id, OrderEvent::Fill(f.clone()));
    h.report(id, OrderEvent::Fill(f));
    assert_eq!(h.oms.position(h.id()).unwrap().qty, 60);
    let fills = h
        .oms
        .take_events()
        .iter()
        .filter(|e| matches!(e, OmsEvent::Filled { .. }))
        .count();
    assert_eq!(fills, 1);
}

#[test]
fn rejected_entry_frees_the_stock() {
    let mut h = H::new();
    let id = h.enter();
    h.report(
        id,
        OrderEvent::Rejected {
            reason: "margin".into(),
        },
    );
    assert!(!h.oms.is_busy(h.id()));
    let events = h.oms.take_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OmsEvent::OrderFailed { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OmsEvent::EntryFinished { filled: 0, .. }))
    );
}

#[test]
fn unknown_outcome_is_looked_up_never_resent() {
    let mut h = H::new();
    let id = h.enter();
    h.oms
        .on_submit_result(id, &Err(ExecError::OutcomeUnknown("timeout".into())), now());
    assert_eq!(h.commands(), [Command::Lookup(id)]);
    assert!(h.oms.is_busy(h.id()), "must stay busy until settled");

    // The lookup itself fails: asked again next tick, once.
    h.oms
        .on_lookup_result(id, &Err(ExecError::Unavailable("down".into())), now());
    assert!(h.commands().is_empty());
    h.oms.tick();
    assert_eq!(h.commands(), [Command::Lookup(id)]);

    // The broker never got it.
    h.oms.on_lookup_result(id, &Ok(None), now());
    assert!(!h.oms.is_busy(h.id()));
    h.oms.tick();
    assert!(h.commands().is_empty());
}

#[test]
fn unknown_outcome_that_was_placed_stays_live() {
    let mut h = H::new();
    let id = h.enter();
    h.oms
        .on_submit_result(id, &Err(ExecError::OutcomeUnknown("timeout".into())), now());
    h.commands();
    let snapshot = OrderSnapshot {
        client_id: id,
        broker_order_id: "b1".into(),
        instrument: h.id(),
        status: OrderStatus::Open,
        filled_qty: 0,
        avg_fill_price: None,
    };
    h.oms.on_lookup_result(id, &Ok(Some(snapshot)), now());
    assert!(h.oms.is_busy(h.id()));
    h.fill(id, Side::Buy, 60, p(500));
    assert_eq!(h.oms.position(h.id()).unwrap().qty, 60);
}

#[test]
fn clear_failures_end_the_order() {
    let mut h = H::new();
    let id = h.enter();
    h.oms
        .on_submit_result(id, &Err(ExecError::RateLimited), now());
    assert!(!h.oms.is_busy(h.id()));
    assert!(h.commands().is_empty());
}

#[test]
fn exit_cancels_first_then_sells_what_is_held() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 60, p(500));
    h.oms.exit(h.id(), now());
    assert_eq!(h.commands(), [Command::Cancel(id)]);
    // Repeats do nothing while that is in flight.
    h.oms.exit(h.id(), now());
    assert!(h.commands().is_empty());

    h.report(id, OrderEvent::Cancelled);
    let cmds = h.commands();
    let Command::Submit(r) = &cmds[0] else {
        panic!("{cmds:?}")
    };
    assert_eq!((r.side, r.qty, r.kind), (Side::Sell, 60, OrderKind::Market));
    assert_eq!(r.bracket, None);
    let exit_id = r.client_id;

    h.oms.exit(h.id(), now());
    assert!(h.commands().is_empty());
    h.fill(exit_id, Side::Sell, 60, p(505));
    assert!(!h.oms.is_busy(h.id()));
}

#[test]
fn stop_firing_during_the_cancel_means_no_extra_order() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 60, p(500));
    h.oms.exit(h.id(), now());
    h.commands();
    h.fill(id, Side::Sell, 60, p(495));
    h.oms
        .on_cancel_result(id, &Err(ExecError::Rejected("finished".into())));
    assert!(!h.oms.is_busy(h.id()));
    // Only a harmless lookup, no sell order.
    assert!(!h.commands().iter().any(|c| matches!(c, Command::Submit(_))));
}

#[test]
fn exit_of_an_unfilled_entry_only_cancels() {
    let mut h = H::new();
    let id = h.enter();
    h.oms.exit(h.id(), now());
    assert_eq!(h.commands(), [Command::Cancel(id)]);
    h.report(id, OrderEvent::Cancelled);
    assert!(h.commands().is_empty());
    assert!(!h.oms.is_busy(h.id()));
    assert_eq!(h.entry_finished(), [0]);
}

#[test]
fn partly_filled_entry_is_cancelled_then_closed() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 20, p(500));
    h.oms.exit(h.id(), now());
    h.commands();
    h.report(id, OrderEvent::Cancelled);
    let cmds = h.commands();
    let Command::Submit(r) = &cmds[0] else {
        panic!("{cmds:?}")
    };
    assert_eq!((r.side, r.qty), (Side::Sell, 20));
}

#[test]
fn failed_cancel_is_looked_up_and_can_be_retried_if_still_live() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 60, p(500));
    h.oms.exit(h.id(), now());
    h.commands();
    h.oms
        .on_cancel_result(id, &Err(ExecError::OutcomeUnknown("x".into())));
    assert_eq!(h.commands(), [Command::Lookup(id)]);
    let mut snapshot = OrderSnapshot {
        client_id: id,
        broker_order_id: "b1".into(),
        instrument: h.id(),
        status: OrderStatus::Open,
        filled_qty: 60,
        avg_fill_price: Some(p(500)),
    };
    h.oms
        .on_lookup_result(id, &Ok(Some(snapshot.clone())), now());
    assert!(h.commands().is_empty());
    h.oms.exit(h.id(), now());
    assert_eq!(h.commands(), [Command::Cancel(id)]);

    // This time the broker says the legs are gone: close at market.
    h.oms
        .on_cancel_result(id, &Err(ExecError::Rejected("gone".into())));
    h.commands();
    snapshot.status = OrderStatus::Filled;
    h.oms.on_lookup_result(id, &Ok(Some(snapshot)), now());
    let cmds = h.commands();
    assert!(matches!(&cmds[..], [Command::Submit(r)] if r.side == Side::Sell && r.qty == 60));
}

#[test]
fn exit_order_that_fails_can_be_retried() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 60, p(500));
    h.oms.exit(h.id(), now());
    h.commands();
    h.report(id, OrderEvent::Cancelled);
    let cmds = h.commands();
    let Command::Submit(r) = &cmds[0] else {
        panic!()
    };
    h.oms
        .on_submit_result(r.client_id, &Err(ExecError::RateLimited), now());
    assert_eq!(h.oms.position(h.id()).unwrap().qty, 60);
    h.oms.exit(h.id(), now());
    assert!(matches!(h.commands()[..], [Command::Submit(_)]));
}

#[test]
fn stop_moves_at_the_broker() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 60, p(500));
    h.oms.adjust_stop(h.id(), p(500));
    assert_eq!(
        h.commands(),
        [Command::Modify {
            id,
            change: OrderChange {
                trigger: Some(p(500)),
                ..OrderChange::default()
            }
        }]
    );
    h.oms.take_events();
    h.oms
        .on_modify_result(id, &Err(ExecError::Rejected("no".into())));
    assert_eq!(h.oms.take_events(), [OmsEvent::StopNotMoved(h.id())]);
}

#[test]
fn flatten_all_covers_positions_and_working_orders() {
    let mut h = H::new();
    let id = h.enter();
    h.oms.flatten_all(now());
    assert_eq!(h.commands(), [Command::Cancel(id)]);
}

#[test]
fn a_fill_for_an_unknown_order_still_counts() {
    let mut h = H::new();
    let id = ClientOrderId::new("stranger").unwrap();
    let f = h.make_fill(id, Side::Buy, 5, p(500), "z");
    h.report(id, OrderEvent::Fill(f));
    assert_eq!(h.oms.position(h.id()).unwrap().qty, 5);
    assert!(
        h.oms
            .take_events()
            .contains(&OmsEvent::UnknownOrderReport(id))
    );
    // And it can be closed.
    h.oms.exit(h.id(), now());
    assert!(matches!(&h.commands()[..], [Command::Submit(r)] if r.qty == 5));
}

#[test]
fn restore_and_reconcile() {
    let mut h = H::new();
    let broker = [PositionSnapshot {
        instrument: h.id(),
        qty: -30,
        avg_price: Some(p(400)),
    }];
    assert_eq!(h.oms.reconcile(&broker).len(), 1);
    h.oms.restore(&broker, now()).unwrap();
    assert!(h.oms.reconcile(&broker).is_empty());
    assert_eq!(h.oms.position(h.id()).unwrap().qty, -30);
    // No strategy owns it, but it is still closed when asked.
    assert_eq!(h.oms.view(StrategyId(3), h.id()).0, 0);
    h.oms.exit(h.id(), now());
    assert!(matches!(&h.commands()[..], [Command::Submit(r)] if r.side == Side::Buy));
    assert_eq!(
        h.oms.reconcile(&[]),
        [Mismatch {
            instrument: h.id(),
            ours: -30,
            broker: 0
        }]
    );
}

#[test]
fn new_day_forgets_realized_and_trades() {
    let mut h = H::new();
    let id = h.enter();
    h.fill(id, Side::Buy, 60, p(500));
    h.fill(id, Side::Sell, 60, p(505));
    assert_ne!(h.oms.view(StrategyId(3), h.id()).2, Money::ZERO);
    h.oms.begin_day();
    assert_eq!(h.oms.view(StrategyId(3), h.id()).2, Money::ZERO);
}
