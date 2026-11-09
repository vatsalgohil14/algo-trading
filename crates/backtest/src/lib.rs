//! Runs strategies over history, through the same [`Engine`] as live
//! trading, with the pessimistic simulated broker in place of a real one.
//!
//! ```text
//! bars -> SimBroker (fills) -> Engine (indicators, strategy, risk, orders)
//!            ^                     |
//!            +---- commands -------+
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod config;
mod report;
mod source;

pub use config::{BacktestConfig, BrokerConfig, DataConfig, StrategyConfig};
pub use report::{Group, Summary, write_daily_csv, write_trades_csv};
pub use source::{BarSource, QuestDbSource, SyntheticSource, load_universe_file};

use chrono::NaiveDate;
use costs::CostConfig;
use engine::{ClosedTrade, Command, Engine, EngineError, EngineSetup, EngineStats, StrategySpec};
use risk::RiskConfig;
use sim::broker::{Faults, SimBroker, SimBrokerConfig};
use trading_core::{Bar, Exchange, InstrumentId, MarketHours, Money, Price, Timestamp, Universe};

#[derive(Debug, thiserror::Error)]
pub enum BacktestError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error(transparent)]
    Core(#[from] trading_core::CoreError),
    #[error("bad config: {0}")]
    Config(String),
    #[error("cannot read {path}: {why}")]
    File { path: String, why: String },
}

/// One trading day's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayResult {
    pub date: NaiveDate,
    pub pnl: Money,
}

/// Everything a run produced.
#[derive(Debug, Clone)]
pub struct BacktestResult {
    pub capital: Money,
    pub symbols: Vec<String>,
    pub trades: Vec<ClosedTrade>,
    pub days: Vec<DayResult>,
    pub filter_counts: Vec<(&'static str, Vec<(&'static str, u64)>)>,
    pub risk_rejections: Vec<(&'static str, u64)>,
    pub stats: EngineStats,
    /// Bars the engine refused (invalid or out of order).
    pub bad_bars: u64,
    /// Positions the simulated broker had to close at the end of a day.
    pub auto_square_offs: u64,
    /// Days on which our positions differed from the broker's at the close.
    pub reconcile_failures: u64,
    /// The broker's own realized P&L before charges, as a cross-check.
    pub broker_realized: Money,
}

/// What to run.
#[derive(Debug, Clone)]
pub struct RunSetup {
    pub universe: Universe,
    pub risk: RiskConfig,
    pub costs: CostConfig,
    pub strategies: Vec<StrategySpec>,
    pub seed: u64,
    pub max_participation: f64,
    /// Multiplies the slippage of simulated fills, to test how much the
    /// results depend on it.
    pub slippage_multiplier: f64,
    pub faults: Faults,
}

/// Safety limit on command rounds per step, against a feedback loop.
const MAX_PUMP_ROUNDS: u32 = 1_000;

struct Runner {
    engine: Engine,
    broker: SimBroker,
}

impl Runner {
    fn deliver(&mut self, now: Timestamp) {
        for report in self.broker.take_reports() {
            self.engine.on_report(&report, now);
        }
    }

    fn pump(&mut self, now: Timestamp) {
        self.deliver(now);
        for _ in 0..MAX_PUMP_ROUNDS {
            let commands = self.engine.take_commands();
            if commands.is_empty() {
                return;
            }
            for command in commands {
                self.carry_out(command, now);
            }
            self.deliver(now);
        }
    }

    fn carry_out(&mut self, command: Command, now: Timestamp) {
        match command {
            Command::Submit(request) => {
                let result = self.broker.submit(&request, now);
                self.engine
                    .on_submit_result(request.client_id, &result, now);
            }
            Command::Cancel(id) => {
                let result = self.broker.cancel(id, now);
                self.engine.on_cancel_result(id, &result, now);
            }
            Command::Modify { id, change } => {
                let result = self.broker.modify(id, change, now);
                self.engine.on_modify_result(id, &result, now);
            }
            Command::Lookup(id) => {
                let result = Ok(self.broker.order_status(id));
                self.engine.on_lookup_result(id, &result, now);
            }
        }
    }
}

/// Runs a backtest over every session `source` provides.
pub fn run(setup: RunSetup, source: &mut dyn BarSource) -> Result<BacktestResult, BacktestError> {
    let ticks: Vec<Price> = setup.universe.iter().map(|i| i.tick_size).collect();
    let symbols: Vec<String> = setup
        .universe
        .iter()
        .map(|i| i.symbol.to_string())
        .collect();
    let capital = Money::from_rupees_f64(setup.risk.capital)
        .ok_or_else(|| BacktestError::Config("capital is too large".into()))?;
    let m = setup.slippage_multiplier;
    if !m.is_finite() || m < 0.0 {
        return Err(BacktestError::Config(
            "slippage_multiplier must be a number >= 0".into(),
        ));
    }
    let mut slippage = setup.costs.slippage.clone();
    slippage.pct *= m;
    slippage.min_ticks = (f64::from(slippage.min_ticks) * m).round() as u32;

    let engine = Engine::new(EngineSetup {
        universe: setup.universe,
        hours: MarketHours::NSE_EQUITY,
        risk: setup.risk,
        costs: setup.costs,
        strategies: setup.strategies,
        boot: 1,
    })?;
    let broker = SimBroker::new(
        SimBrokerConfig {
            seed: setup.seed,
            slippage,
            max_participation: setup.max_participation,
            faults: setup.faults,
        },
        ticks,
    );
    let mut runner = Runner { engine, broker };
    let mut trades = Vec::new();
    let mut days = Vec::new();
    let mut bad_bars = 0;
    let mut reconcile_failures = 0;

    while let Some((date, bars)) = source.next_session()? {
        let Some(last) = bars.last().map(|b: &Bar| b.ts.plus_minutes(1)) else {
            continue;
        };
        let mut minute: Option<Timestamp> = None;
        for bar in &bars {
            if let Some(previous) = minute.filter(|m| *m != bar.ts) {
                end_minute(&mut runner, previous.plus_minutes(1));
            }
            minute = Some(bar.ts);
            let close = bar.ts.plus_minutes(1);
            runner.broker.on_bar(bar);
            runner.deliver(close);
            if runner.engine.on_bar(bar).is_err() {
                bad_bars += 1;
            }
            runner.pump(close);
        }
        if minute.is_some() {
            end_minute(&mut runner, last);
        }

        runner.broker.end_of_day(last);
        runner.pump(last);
        if !runner
            .engine
            .reconcile(&runner.broker.positions())
            .is_empty()
        {
            reconcile_failures += 1;
        }
        days.push(DayResult {
            date,
            pnl: runner.engine.risk().day_pnl(),
        });
        trades.extend(runner.engine.take_closed_trades());
    }

    Ok(BacktestResult {
        capital,
        symbols,
        trades,
        days,
        filter_counts: runner.engine.filter_counts(),
        risk_rejections: runner.engine.risk().rejections().collect(),
        stats: runner.engine.stats(),
        bad_bars,
        auto_square_offs: runner.broker.auto_square_offs(),
        reconcile_failures,
        broker_realized: runner.broker.total_realized(),
    })
}

fn end_minute(runner: &mut Runner, close: Timestamp) {
    runner.engine.on_minute_end(close);
    runner.pump(close);
}

/// Resolves a config's strategies against a universe.
pub fn strategy_specs(
    strategies: &[StrategyConfig],
    universe: &Universe,
) -> Result<Vec<StrategySpec>, BacktestError> {
    strategies
        .iter()
        .map(|s| {
            let stocks = match &s.stocks {
                None => None,
                Some(names) => Some(
                    names
                        .iter()
                        .map(|name| {
                            universe
                                .by_symbol(Exchange::Nse, name)
                                .map(|i| i.id)
                                .ok_or_else(|| {
                                    BacktestError::Config(format!(
                                        "strategy {} lists unknown stock {name}",
                                        s.name
                                    ))
                                })
                        })
                        .collect::<Result<Vec<InstrumentId>, _>>()?,
                ),
            };
            Ok(StrategySpec {
                name: s.name.clone(),
                stocks,
                params: s.params.clone(),
            })
        })
        .collect()
}
