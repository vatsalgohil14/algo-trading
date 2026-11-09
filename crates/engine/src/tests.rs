use chrono::NaiveDate;
use risk::Halt;
use trading_core::{Exchange, Fill, OrderEvent, OrderRequest, Side};

use super::*;

fn p(rupees: f64) -> Price {
    Price::from_rupees_f64(rupees).unwrap()
}

fn m(rupees: f64) -> Money {
    Money::from_rupees_f64(rupees).unwrap()
}

fn open() -> Timestamp {
    let date = NaiveDate::from_ymd_opt(2025, 1, 9).unwrap();
    MarketHours::NSE_EQUITY.open_at(date).unwrap()
}

fn t(minute: i64) -> Timestamp {
    open().plus_minutes(minute)
}

const A: InstrumentId = InstrumentId(0);
const OWNER: StrategyId = StrategyId(0);

struct H {
    e: Engine,
    trade: u32,
}

impl H {
    fn new() -> Self {
        let mut universe = Universe::new();
        for i in 0..2 {
            universe
                .add(Exchange::Nse, &format!("S{i}"), i, Price::from_paise(5), 1)
                .unwrap();
        }
        let e = Engine::new(EngineSetup {
            universe,
            hours: MarketHours::NSE_EQUITY,
            risk: RiskConfig::default(),
            costs: CostConfig::default(),
            strategies: Vec::new(),
            boot: 1,
        })
        .unwrap();
        Self { e, trade: 0 }
    }

    fn signal(&mut self, s: Signal) -> Vec<Command> {
        let now = s.ts;
        self.e.process(&s, now);
        self.e.take_commands()
    }

    /// Enters 60 shares at 500 and reports the fill.
    fn enter(&mut self) -> OrderRequest {
        let cmds = self.signal(long());
        let Some(Command::Submit(r)) = cmds.into_iter().next() else {
            panic!("no entry sent");
        };
        self.fill(&r.client_id, Side::Buy, r.qty, 500.0);
        r
    }

    fn fill(&mut self, id: &ClientOrderId, side: Side, qty: u32, price: f64) {
        self.trade += 1;
        let fill = Fill {
            client_id: *id,
            instrument: A,
            side,
            qty,
            price: p(price),
            ts: t(61),
            trade_id: format!("t{}", self.trade).into(),
        };
        self.report(*id, OrderEvent::Fill(fill));
    }

    fn report(&mut self, id: ClientOrderId, event: OrderEvent) {
        self.e.on_report(
            &ExecutionReport {
                client_id: id,
                broker_order_id: None,
                ts: t(61),
                event,
            },
            t(62),
        );
    }
}

fn long() -> Signal {
    Signal {
        strategy: OWNER,
        instrument: A,
        ts: t(60),
        intent: Intent::EnterLong,
        entry: Some(p(500.0)),
        stop: Some(p(495.0)),
        target: Some(p(510.0)),
        reason: "setup",
    }
}

fn exit_by(strategy: StrategyId) -> Signal {
    Signal {
        strategy,
        intent: Intent::Exit,
        entry: None,
        stop: None,
        target: None,
        reason: "my_exit",
        ts: t(62),
        ..long()
    }
}

fn bar(minute: i64, close: f64) -> Bar {
    let c = p(close);
    Bar {
        instrument: A,
        ts: t(minute),
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 1000,
    }
}

#[test]
fn approved_entry_is_sized_and_sent_with_its_stop() {
    let mut h = H::new();
    let cmds = h.signal(long());
    let [Command::Submit(r)] = &cmds[..] else {
        panic!("{cmds:?}")
    };
    assert_eq!(r.qty, 60);
    assert_eq!(r.bracket.unwrap().stop, p(495.0));
    assert_eq!(h.e.stats().entries_sent, 1);
    assert_eq!(h.e.risk().gross_exposure(), m(30_000.0));
}

#[test]
fn refused_by_risk_sends_nothing() {
    let mut h = H::new();
    h.signal(long());
    assert!(h.signal(long()).is_empty(), "one position per stock");
}

#[test]
fn entries_only_when_conditions_allow() {
    let mut h = H::new();
    let late = Signal {
        ts: t(330), // 14:45
        ..long()
    };
    assert!(h.signal(late).is_empty());
    assert_eq!(h.e.stats().entries_outside_window, 1);

    h.e.set_feed_ok(false);
    assert!(h.signal(long()).is_empty());
    assert_eq!(h.e.stats().entries_feed_down, 1);
    h.e.set_feed_ok(true);

    h.e.set_tradable(A, false);
    assert!(h.signal(long()).is_empty());
    assert_eq!(h.e.stats().entries_not_tradable, 1);
    h.e.set_tradable(A, true);
    assert_eq!(h.signal(long()).len(), 1);
}

#[test]
fn a_strategy_cannot_touch_another_strategys_trade() {
    let mut h = H::new();
    let entry = h.enter();
    let other = StrategyId(1);
    assert!(h.signal(exit_by(other)).is_empty());
    let adjust = Signal {
        strategy: other,
        intent: Intent::AdjustStop,
        stop: Some(p(499.0)),
        ..long()
    };
    assert!(h.signal(adjust).is_empty());
    assert_eq!(h.e.stats().signals_for_a_trade_not_owned, 2);

    assert_eq!(h.signal(exit_by(OWNER)), [Command::Cancel(entry.client_id)]);
}

#[test]
fn stop_moves_only_if_risk_allows() {
    let mut h = H::new();
    let entry = h.enter();
    let adjust = |stop: f64| Signal {
        intent: Intent::AdjustStop,
        stop: Some(p(stop)),
        ..long()
    };
    assert!(h.signal(adjust(490.0)).is_empty(), "widening is refused");
    let cmds = h.signal(adjust(500.0));
    assert!(matches!(&cmds[..], [Command::Modify { id, .. }] if *id == entry.client_id));
}

#[test]
fn square_off_runs_from_the_clock_without_any_bars() {
    let mut h = H::new();
    let entry = h.enter();
    h.e.on_clock(t(354)); // 15:09
    assert!(h.e.take_commands().is_empty());
    h.e.on_clock(t(355)); // 15:10
    assert_eq!(h.e.take_commands(), [Command::Cancel(entry.client_id)]);
}

#[test]
fn daily_loss_closes_everything_and_stops_entries() {
    let mut h = H::new();
    let entry = h.enter();
    h.e.on_bar(&bar(62, 333.0)).unwrap(); // -₹10,020 on 60 shares
    assert_eq!(h.e.take_commands(), [Command::Cancel(entry.client_id)]);
    assert_eq!(h.e.stats().daily_loss_halts, 1);
    assert_eq!(h.e.risk().halted(), Some(Halt::DailyLoss));
    let other = Signal {
        instrument: InstrumentId(1),
        ..long()
    };
    assert!(h.signal(other).is_empty());
}

#[test]
fn kill_switch_closes_everything() {
    let mut h = H::new();
    let entry = h.enter();
    h.e.stop_trading(t(62));
    assert_eq!(h.e.take_commands(), [Command::Cancel(entry.client_id)]);
    assert_eq!(h.e.risk().halted(), Some(Halt::Manual));
}

#[test]
fn a_finished_trade_reaches_the_journal() {
    let mut h = H::new();
    let entry = h.enter();
    h.signal(exit_by(OWNER));
    h.report(entry.client_id, OrderEvent::Cancelled);
    let cmds = h.e.take_commands();
    let [Command::Submit(exit)] = &cmds[..] else {
        panic!("{cmds:?}")
    };
    h.fill(&exit.client_id, Side::Sell, 60, 505.0);

    let trades = h.e.take_closed_trades();
    let [t] = &trades[..] else {
        panic!("{trades:?}")
    };
    assert_eq!((t.setup, t.exit_reason), ("setup", "my_exit"));
    assert_eq!((t.side, t.qty), (Side::Buy, 60));
    assert_eq!((t.entry_price, t.exit_price), (p(500.0), p(505.0)));
    assert_eq!(t.gross, m(300.0));
    assert_eq!(t.net, t.gross - t.charges);
    assert_eq!(t.net, h.e.risk().day_pnl());
    assert_eq!(t.planned_risk, m(300.0));
    assert!((t.r_multiple().unwrap() - t.net.as_f64() / 300.0).abs() < 1e-9);
}

#[test]
fn a_trade_closed_at_the_broker_is_labelled() {
    let mut h = H::new();
    let entry = h.enter();
    h.fill(&entry.client_id, Side::Sell, 60, 495.0);
    let trades = h.e.take_closed_trades();
    assert_eq!(trades[0].exit_reason, "stop_or_target");
    assert_eq!(trades[0].gross, m(-300.0));
}

#[test]
fn restart_keeps_positions_for_the_clock_to_close() {
    let mut h = H::new();
    let broker = [PositionSnapshot {
        instrument: A,
        qty: 60,
        avg_price: Some(p(500.0)),
    }];
    h.e.restore(&broker, m(-1_000.0), t(100)).unwrap();
    assert!(h.e.reconcile(&broker).is_empty());
    assert_eq!(h.e.risk().day_pnl(), m(-1_000.0));
    h.e.on_clock(t(355));
    let cmds = h.e.take_commands();
    assert!(matches!(&cmds[..], [Command::Submit(r)] if r.side == Side::Sell && r.qty == 60));
}

#[test]
fn bad_bars_are_refused() {
    let mut h = H::new();
    let mut b = bar(1, 500.0);
    b.high = p(100.0);
    assert!(h.e.on_bar(&b).is_err());
    assert!(h.e.on_bar(&bar(1, 500.0)).is_ok());
    assert!(h.e.on_bar(&bar(1, 500.0)).is_err(), "duplicate");
}

#[test]
fn unknown_strategy_stops_start_up() {
    let result = Engine::new(EngineSetup {
        universe: Universe::new(),
        hours: MarketHours::NSE_EQUITY,
        risk: RiskConfig::default(),
        costs: CostConfig::default(),
        strategies: vec![StrategySpec {
            name: "nope".into(),
            stocks: None,
            params: serde_json::json!({}),
        }],
        boot: 1,
    });
    assert!(result.is_err());
}
