//! The whole system on synthetic data: strategy, risk, orders, simulated
//! broker. Synthetic data has no edge, so these check the accounting and
//! the failure handling, not whether anything makes money.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use backtest::{BacktestResult, RunSetup, Summary, SyntheticSource, run};
use chrono::NaiveDate;
use costs::CostConfig;
use engine::StrategySpec;
use risk::RiskConfig;
use sim::broker::Faults;
use sim::synthetic::sample_universe;
use trading_core::{Exchange, Money, Universe};

fn backtest(faults: Faults, slippage_multiplier: f64) -> BacktestResult {
    let orb = StrategySpec {
        name: "orb".into(),
        stocks: None,
        params: serde_json::json!({"min_turnover_cr": 5.0}),
    };
    backtest_with(vec![orb], faults, slippage_multiplier)
}

fn backtest_with(
    strategies: Vec<StrategySpec>,
    faults: Faults,
    slippage_multiplier: f64,
) -> BacktestResult {
    let profiles = sample_universe(25, 3);
    let mut universe = Universe::new();
    for p in &profiles {
        universe
            .add(Exchange::Nse, &p.symbol, p.instrument.0, p.tick_size, 1)
            .unwrap();
    }
    let from = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let to = NaiveDate::from_ymd_opt(2025, 5, 30).unwrap();
    let mut source = SyntheticSource::new(profiles, 9, from, to).unwrap();
    run(
        RunSetup {
            universe,
            risk: RiskConfig::default(),
            costs: CostConfig::default(),
            strategies,
            seed: 5,
            max_participation: 0.10,
            slippage_multiplier,
            faults,
        },
        &mut source,
    )
    .unwrap()
}

fn net(r: &BacktestResult) -> Money {
    r.trades.iter().map(|t| t.net).sum()
}

fn charges(r: &BacktestResult) -> Money {
    r.trades.iter().map(|t| t.charges).sum()
}

fn day_total(r: &BacktestResult) -> Money {
    r.days.iter().map(|d| d.pnl).sum()
}

/// Three independent sets of books must agree: the trade journal, the risk
/// manager's daily P&L, and the simulated broker's own positions.
fn books_agree(r: &BacktestResult) {
    assert_eq!(net(r), day_total(r), "journal vs risk manager");
    assert_eq!(net(r), r.broker_realized - charges(r), "journal vs broker");
    assert_eq!(r.reconcile_failures, 0, "positions differ from the broker");
    assert_eq!(r.bad_bars, 0);
}

#[test]
fn clean_run_balances_and_closes_everything() {
    let r = backtest(Faults::default(), 1.0);
    assert!(r.trades.len() > 100, "only {} trades", r.trades.len());
    books_agree(&r);
    assert_eq!(r.auto_square_offs, 0, "the system must flatten by itself");
    assert_eq!(r.stats.orders_failed, 0);

    for t in &r.trades {
        assert!(t.qty > 0);
        let exposure = t.entry_price.notional(t.qty);
        assert!(
            exposure <= Money::from_rupees_f64(50_000.0).unwrap(),
            "{t:?}"
        );
        // Slippage aside, no trade can plan to lose more than the cap.
        assert!(t.planned_risk <= Money::from_rupees_f64(500.0).unwrap());
        assert!(t.closed >= t.opened);
    }

    let s = Summary::of(&r);
    assert_eq!(s.overall.net, net(&r));
    assert!(s.render(&r).unwrap().contains("PASS MARK"));
}

#[test]
fn same_input_same_result() {
    let a = backtest(Faults::default(), 1.0);
    let b = backtest(Faults::default(), 1.0);
    assert_eq!(a.trades, b.trades);
    assert_eq!(a.days, b.days);
}

#[test]
fn survives_a_badly_behaved_broker() {
    let r = backtest(
        Faults {
            reject: 0.05,
            unknown_not_placed: 0.05,
            unknown_placed: 0.05,
            duplicate_fill: 0.25,
            cancel_fails: 0.10,
        },
        1.0,
    );
    assert!(r.trades.len() > 50, "only {} trades", r.trades.len());
    assert!(r.stats.orders_failed > 0, "faults were not exercised");
    books_agree(&r);
}

/// The path a new strategy takes: named in the config, run with no other
/// changes, alongside another strategy on the same stocks.
#[test]
fn a_second_strategy_plugs_in_by_name() {
    let spec = |name: &str| StrategySpec {
        name: name.into(),
        stocks: None,
        params: serde_json::json!({}),
    };
    let alone = backtest_with(vec![spec("template")], Faults::default(), 1.0);
    assert!(alone.trades.iter().all(|t| t.setup == "template_long"));
    assert!(
        alone.trades.len() > 20,
        "only {} trades",
        alone.trades.len()
    );
    books_agree(&alone);

    let orb = StrategySpec {
        params: serde_json::json!({"min_turnover_cr": 5.0}),
        ..spec("orb")
    };
    let both = backtest_with(vec![orb, spec("template")], Faults::default(), 1.0);
    books_agree(&both);
    assert_eq!(both.auto_square_offs, 0);
    assert!(both.trades.iter().any(|t| t.setup == "template_long"));
    assert!(both.trades.iter().any(|t| t.setup.starts_with("orb_")));
}

#[test]
fn more_slippage_costs_more() {
    let normal = backtest(Faults::default(), 1.0);
    let doubled = backtest(Faults::default(), 2.0);
    assert!(net(&doubled) < net(&normal));
    books_agree(&doubled);
}
