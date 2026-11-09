//! Each rule with worked numbers. Instruments tick in 5 paise, lot 1.

use trading_core::{
    ClientOrderId, Exchange, Fill, Instrument, InstrumentId, Intent, Money, PositionSnapshot,
    Price, Side, Signal, StrategyId, Timestamp, Universe,
};

use crate::*;
use costs::CostModel;

fn p(rupees: f64) -> Price {
    Price::from_rupees_f64(rupees).unwrap()
}

fn m(rupees: f64) -> Money {
    Money::from_rupees_f64(rupees).unwrap()
}

/// A moment during the 2025-01-09 session; only the day matters here.
fn now() -> Timestamp {
    Timestamp::from_secs(1_736_400_000)
}

struct H {
    risk: RiskManager,
    universe: Universe,
}

impl H {
    fn new(config: RiskConfig) -> Self {
        let mut universe = Universe::new();
        for i in 0..4 {
            universe
                .add(Exchange::Nse, &format!("S{i}"), i, Price::from_paise(5), 1)
                .unwrap();
        }
        Self {
            risk: RiskManager::new(&config, CostModel::default(), 4).unwrap(),
            universe,
        }
    }

    fn inst(&self, i: u32) -> Instrument {
        self.universe.get(InstrumentId(i)).unwrap().clone()
    }

    fn eval(&mut self, s: Signal) -> Decision {
        let inst = self.inst(s.instrument.0);
        self.risk.evaluate(&s, &inst, Price::ZERO)
    }

    fn approved(&mut self, s: Signal) -> Approved {
        match self.eval(s) {
            Decision::Enter(a) => a,
            other => panic!("expected approval, got {other:?}"),
        }
    }

    fn rejected(&mut self, s: Signal) -> RiskReject {
        match self.eval(s) {
            Decision::Reject(r) => r,
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    fn fill(&mut self, i: u32, side: Side, qty: u32, price: f64) -> Option<RiskEvent> {
        self.risk.on_fill(&Fill {
            client_id: ClientOrderId::new("t").unwrap(),
            instrument: InstrumentId(i),
            side,
            qty,
            price: p(price),
            ts: now(),
            trade_id: "x".into(),
        })
    }
}

fn long(i: u32, entry: f64, stop: f64, target: f64) -> Signal {
    Signal {
        strategy: StrategyId(0),
        instrument: InstrumentId(i),
        ts: now(),
        intent: Intent::EnterLong,
        entry: Some(p(entry)),
        stop: Some(p(stop)),
        target: Some(p(target)),
        reason: "setup",
    }
}

fn short(i: u32, entry: f64, stop: f64, target: f64) -> Signal {
    Signal {
        intent: Intent::EnterShort,
        ..long(i, entry, stop, target)
    }
}

fn scored(score: f64) -> RiskConfig {
    let mut c = RiskConfig::default();
    c.setup_scores.insert("setup".into(), score);
    c
}

#[test]
fn unproven_setup_gets_the_minimum_size() {
    let mut h = H::new(RiskConfig::default());
    let a = h.approved(long(0, 503.05, 498.0, 513.15));
    // 30,000 / 503.05 = 59.6, so 60 shares reach the minimum.
    assert_eq!(a.qty, 60);
    assert_eq!(a.side, Side::Buy);
    assert_eq!(a.exposure, m(30_183.0));
    assert_eq!(a.margin, m(6_036.6)); // at 5x
    assert_eq!(a.max_loss, m(303.0));
}

#[test]
fn score_moves_size_across_the_range() {
    let mut h = H::new(scored(0.5));
    // 40,000 / 503.05 = 79.5.
    assert_eq!(h.approved(long(0, 503.05, 498.0, 513.15)).qty, 79);
    let mut h = H::new(scored(1.0));
    let a = h.approved(long(0, 503.05, 498.0, 513.15));
    assert_eq!(a.qty, 99); // 50,000 / 503.05 = 99.4
    assert!(a.exposure <= m(50_000.0));
}

#[test]
fn size_shrinks_to_the_loss_cap() {
    let mut h = H::new(scored(1.0));
    // ₹6 of risk a share: 500 / 6 = 83 shares, ₹41,500, not 100.
    let a = h.approved(long(0, 500.0, 494.0, 512.0));
    assert_eq!(a.qty, 83);
    assert_eq!(a.exposure, m(41_500.0));
    assert_eq!(a.max_loss, m(498.0));
}

#[test]
fn trade_is_skipped_if_the_loss_cap_breaks_the_minimum() {
    let mut h = H::new(RiskConfig::default());
    // ₹10 of risk: at most 50 shares = ₹25,000, below the ₹30,000 minimum.
    assert_eq!(
        h.rejected(long(0, 500.0, 490.0, 520.0)),
        RiskReject::LossCap
    );
}

#[test]
fn stock_above_the_maximum_cannot_be_traded() {
    let mut h = H::new(RiskConfig::default());
    assert_eq!(
        h.rejected(long(0, 60_000.0, 59_990.0, 60_100.0)),
        RiskReject::TooExpensive
    );
    // A dear stock that still fits: one share of ₹45,000.
    let mut h = H::new(RiskConfig::default());
    let a = h.approved(long(0, 45_000.0, 44_900.0, 45_400.0));
    assert_eq!((a.qty, a.exposure), (1, m(45_000.0)));
}

#[test]
fn reward_must_cover_costs_four_times() {
    let mut h = H::new(RiskConfig::default());
    // 60 shares at ₹500 cost about ₹44 round trip, so reward must be about
    // ₹177: a ₹1 target (₹60) is refused, ₹3 (₹180) passes.
    assert_eq!(
        h.rejected(long(0, 500.0, 499.0, 501.0)),
        RiskReject::CostTooHigh
    );
    let a = h.approved(long(0, 500.0, 499.0, 503.0));
    assert_eq!(a.qty, 60);
    let cost = a.expected_cost.paise();
    assert!((4_300..4_600).contains(&cost), "cost {cost}");
}

#[test]
fn market_entry_uses_the_reference_price() {
    let mut h = H::new(RiskConfig::default());
    let inst = h.inst(0);
    let s = Signal {
        entry: None,
        ..long(0, 500.0, 495.0, 510.0)
    };
    assert!(matches!(
        h.risk.evaluate(&s, &inst, Price::ZERO),
        Decision::Reject(RiskReject::InvalidSignal)
    ));
    match h.risk.evaluate(&s, &inst, p(500.0)) {
        Decision::Enter(a) => assert_eq!((a.entry, a.qty), (p(500.0), 60)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn bad_signals_are_refused() {
    let mut h = H::new(RiskConfig::default());
    let no_target = Signal {
        target: None,
        ..long(0, 500.0, 495.0, 510.0)
    };
    assert_eq!(h.rejected(no_target), RiskReject::NoTarget);
    // Stop on the wrong side.
    assert_eq!(
        h.rejected(long(0, 500.0, 505.0, 510.0)),
        RiskReject::InvalidSignal
    );
    // Signal for one stock, instrument data for another.
    let inst = h.inst(1);
    assert_eq!(
        h.risk
            .evaluate(&long(0, 500.0, 495.0, 510.0), &inst, Price::ZERO),
        Decision::Reject(RiskReject::UnknownInstrument)
    );
}

#[test]
fn one_position_per_stock() {
    let mut h = H::new(RiskConfig::default());
    let s = long(0, 500.0, 495.0, 510.0);
    assert_eq!(h.approved(s).qty, 60);
    // Reserved as soon as approved, even in the opposite direction.
    assert_eq!(h.rejected(s), RiskReject::StockBusy);
    assert_eq!(
        h.rejected(short(0, 500.0, 505.0, 490.0)),
        RiskReject::StockBusy
    );
    // Entry never filled: the stock is free again.
    h.risk.release(InstrumentId(0));
    assert_eq!(h.risk.gross_exposure(), Money::ZERO);
    assert_eq!(h.approved(s).qty, 60);

    // Filled: still busy after the reservation is released, free once flat.
    h.fill(0, Side::Buy, 60, 500.0);
    h.risk.release(InstrumentId(0));
    assert_eq!(h.rejected(s), RiskReject::StockBusy);
    h.fill(0, Side::Sell, 60, 505.0);
    assert_eq!(h.approved(s).qty, 60);
}

#[test]
fn partial_fill_is_not_counted_twice() {
    let mut h = H::new(RiskConfig::default());
    h.approved(long(0, 500.0, 495.0, 510.0));
    h.fill(0, Side::Buy, 20, 500.0);
    assert_eq!(h.risk.gross_exposure(), m(30_000.0));
    assert_eq!(h.risk.margin_used(), m(6_000.0));
    h.risk.release(InstrumentId(0));
    assert_eq!(h.risk.gross_exposure(), m(10_000.0));
}

#[test]
fn gross_exposure_cap() {
    let mut h = H::new(RiskConfig {
        max_gross_exposure: 60_000.0,
        ..RiskConfig::default()
    });
    h.approved(long(0, 500.0, 495.0, 510.0));
    assert_eq!(h.risk.gross_exposure(), m(30_000.0));
    h.approved(short(1, 500.0, 505.0, 490.0));
    assert_eq!(h.risk.gross_exposure(), m(60_000.0));
    assert_eq!(h.risk.net_exposure(), Money::ZERO);
    assert_eq!(
        h.rejected(long(2, 500.0, 495.0, 510.0)),
        RiskReject::GrossExposure
    );
}

#[test]
fn net_exposure_cap_blocks_only_the_crowded_side() {
    let mut h = H::new(RiskConfig {
        max_net_exposure: 60_000.0,
        ..RiskConfig::default()
    });
    h.approved(long(0, 500.0, 495.0, 510.0));
    h.approved(long(1, 500.0, 495.0, 510.0));
    assert_eq!(h.risk.net_exposure(), m(60_000.0));
    assert_eq!(
        h.rejected(long(2, 500.0, 495.0, 510.0)),
        RiskReject::NetExposure
    );
    h.approved(short(2, 500.0, 505.0, 490.0));
    assert_eq!(h.risk.net_exposure(), m(30_000.0));
}

#[test]
fn margin_depends_on_the_stock_leverage() {
    let mut h = H::new(RiskConfig {
        capital: 40_000.0,
        daily_loss_limit: 1_000.0,
        ..RiskConfig::default()
    });
    h.risk.set_leverage(InstrumentId(0), 1.0).unwrap();
    let a = h.approved(long(0, 500.0, 495.0, 510.0));
    assert_eq!(a.margin, m(30_000.0));
    // ₹10,000 left; the next trade needs ₹15,000 at 2x but ₹6,000 at 5x.
    h.risk.set_leverage(InstrumentId(1), 2.0).unwrap();
    assert_eq!(h.rejected(long(1, 500.0, 495.0, 510.0)), RiskReject::Margin);
    h.approved(long(2, 500.0, 495.0, 510.0));
    assert!(h.risk.set_leverage(InstrumentId(0), 0.5).is_err());
    assert!(h.risk.set_leverage(InstrumentId(9), 2.0).is_err());
}

#[test]
fn losses_today_reduce_the_margin_available() {
    let mut h = H::new(RiskConfig {
        capital: 12_000.0,
        daily_loss_limit: 5_000.0,
        ..RiskConfig::default()
    });
    h.approved(long(0, 500.0, 495.0, 510.0)); // ₹6,000 margin
    h.fill(0, Side::Buy, 60, 500.0);
    h.risk.release(InstrumentId(0));
    h.risk.on_mark(InstrumentId(0), p(495.0), now()); // -₹300 and charges
    assert_eq!(h.rejected(long(1, 500.0, 495.0, 510.0)), RiskReject::Margin);
}

#[test]
fn pnl_includes_charges_and_open_positions() {
    let mut h = H::new(RiskConfig::default());
    h.fill(0, Side::Buy, 100, 500.0);
    assert_eq!(h.risk.day_pnl(), m(-21.56));
    h.risk.on_mark(InstrumentId(0), p(505.0), now());
    assert_eq!(h.risk.day_pnl(), m(478.44));
    h.fill(0, Side::Sell, 100, 505.0);
    assert_eq!(h.risk.day_pnl(), m(445.67));
}

#[test]
fn daily_loss_halts_new_trades_once() {
    let mut h = H::new(RiskConfig::default());
    h.fill(0, Side::Buy, 100, 500.0);
    // -₹9,970 and ₹21.56 of charges: not yet.
    assert_eq!(h.risk.on_mark(InstrumentId(0), p(400.3), now()), None);
    assert_eq!(h.risk.halted(), None);
    // -₹10,000 and charges: over.
    assert_eq!(
        h.risk.on_mark(InstrumentId(0), p(400.0), now()),
        Some(RiskEvent::DailyLossHit { pnl: m(-10_021.56) })
    );
    assert_eq!(h.risk.halted(), Some(Halt::DailyLoss));
    // Reported once; still halted even if the price recovers.
    assert_eq!(h.risk.on_mark(InstrumentId(0), p(399.0), now()), None);
    h.risk.on_mark(InstrumentId(0), p(500.0), now());
    assert_eq!(h.rejected(long(1, 500.0, 495.0, 510.0)), RiskReject::Halted);
    // Getting out is never blocked.
    let exit = Signal {
        intent: Intent::Exit,
        ..long(0, 500.0, 495.0, 510.0)
    };
    assert_eq!(h.eval(exit), Decision::Allow);
    h.fill(0, Side::Sell, 100, 500.0);

    // A new day starts clean.
    let tomorrow = now().plus_minutes(24 * 60);
    h.risk.on_mark(InstrumentId(0), p(500.0), tomorrow);
    assert_eq!(h.risk.halted(), None);
    assert_eq!(h.risk.day_pnl(), Money::ZERO);
    let next = Signal {
        ts: tomorrow,
        ..long(1, 500.0, 495.0, 510.0)
    };
    assert_eq!(h.approved(next).qty, 60);
}

#[test]
fn manual_halt_stays_until_resumed() {
    let mut h = H::new(RiskConfig::default());
    h.risk.halt();
    assert_eq!(h.risk.halted(), Some(Halt::Manual));
    assert_eq!(h.rejected(long(0, 500.0, 495.0, 510.0)), RiskReject::Halted);
    let tomorrow = now().plus_minutes(24 * 60);
    h.risk.on_mark(InstrumentId(0), p(500.0), tomorrow);
    assert_eq!(h.risk.halted(), Some(Halt::Manual));
    h.risk.resume();
    assert_eq!(h.risk.halted(), None);
}

#[test]
fn stop_can_only_tighten() {
    let adjust = |i: u32, stop: f64| Signal {
        intent: Intent::AdjustStop,
        entry: None,
        target: None,
        stop: Some(p(stop)),
        ..long(i, 500.0, 495.0, 510.0)
    };
    let mut h = H::new(RiskConfig::default());
    assert_eq!(h.rejected(adjust(0, 500.0)), RiskReject::NoOpenTrade);
    h.approved(long(0, 500.0, 495.0, 510.0));
    h.fill(0, Side::Buy, 60, 500.0);
    h.risk.release(InstrumentId(0));
    assert_eq!(h.rejected(adjust(0, 490.0)), RiskReject::StopNotTighter);
    assert_eq!(h.rejected(adjust(0, 495.0)), RiskReject::StopNotTighter);
    assert_eq!(h.eval(adjust(0, 500.0)), Decision::Allow);
    // Measured from the new stop now.
    assert_eq!(h.rejected(adjust(0, 499.0)), RiskReject::StopNotTighter);

    h.approved(short(1, 500.0, 505.0, 490.0));
    h.fill(1, Side::Sell, 60, 500.0);
    assert_eq!(h.rejected(adjust(1, 506.0)), RiskReject::StopNotTighter);
    assert_eq!(h.eval(adjust(1, 501.0)), Decision::Allow);
}

#[test]
fn restart_keeps_positions_and_the_daily_loss() {
    let mut h = H::new(RiskConfig::default());
    let positions = [
        PositionSnapshot {
            instrument: InstrumentId(0),
            qty: 60,
            avg_price: Some(p(500.0)),
        },
        PositionSnapshot {
            instrument: InstrumentId(1),
            qty: -60,
            avg_price: Some(p(400.0)),
        },
    ];
    assert_eq!(
        h.risk.restore(&positions, m(-2_000.0), now()).unwrap(),
        None
    );
    assert_eq!(h.risk.day_pnl(), m(-2_000.0));
    assert_eq!(h.risk.gross_exposure(), m(54_000.0));
    assert_eq!(h.risk.net_exposure(), m(6_000.0));
    assert_eq!(
        h.rejected(long(0, 500.0, 495.0, 510.0)),
        RiskReject::StockBusy
    );

    // A restart cannot wipe out a limit already reached.
    assert_eq!(
        h.risk.restore(&[], m(-10_000.0), now()).unwrap(),
        Some(RiskEvent::DailyLossHit { pnl: m(-10_000.0) })
    );
    assert_eq!(h.risk.halted(), Some(Halt::DailyLoss));

    let no_price = [PositionSnapshot {
        instrument: InstrumentId(0),
        qty: 1,
        avg_price: None,
    }];
    assert!(h.risk.restore(&no_price, Money::ZERO, now()).is_err());
}

#[test]
fn rejections_are_counted() {
    let mut h = H::new(RiskConfig::default());
    h.rejected(long(0, 500.0, 490.0, 520.0));
    h.rejected(long(0, 500.0, 490.0, 520.0));
    let n = h
        .risk
        .rejections()
        .find(|(r, _)| *r == "loss_cap")
        .unwrap()
        .1;
    assert_eq!(n, 2);
}

#[test]
fn bad_config_is_refused() {
    let bad = |f: fn(&mut RiskConfig)| {
        let mut c = RiskConfig::default();
        f(&mut c);
        RiskManager::new(&c, CostModel::default(), 1).is_err()
    };
    assert!(!bad(|_| {}));
    assert!(bad(|c| c.capital = 0.0));
    assert!(bad(|c| c.min_exposure = 60_000.0));
    assert!(bad(|c| c.max_loss_per_trade = f64::NAN));
    assert!(bad(|c| c.default_leverage = 0.5));
    assert!(bad(|c| c.max_gross_exposure = 10_000.0));
    assert!(bad(|c| c.daily_loss_limit = 1e9));
    assert!(bad(|c| {
        c.setup_scores.insert("x".into(), 1.5);
    }));
}

#[test]
fn config_from_json() {
    let c: RiskConfig =
        serde_json::from_str(r#"{"max_exposure": 60000, "setup_scores": {"orb_long": 0.25}}"#)
            .unwrap();
    assert_eq!(c.max_exposure, 60_000.0);
    assert_eq!(c.min_exposure, 30_000.0);
    assert_eq!(c.setup_scores["orb_long"], 0.25);
    assert!(serde_json::from_str::<RiskConfig>(r#"{"max_risk": 1}"#).is_err());
}
