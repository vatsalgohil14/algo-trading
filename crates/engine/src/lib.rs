//! Connects the building blocks. Bars and broker reports go in; commands
//! for the broker come out. It does no I/O and has no clock of its own, so
//! a backtest and live trading run exactly this code.
//!
//! Per minute, a driver:
//!
//! 1. Passes on broker reports with [`Engine::on_report`].
//! 2. Gives each stock's bar to [`Engine::on_bar`].
//! 3. Calls [`Engine::on_minute_end`] once every stock's bar is in.
//! 4. Carries out [`Engine::take_commands`] and returns each answer.
//!
//! Rules enforced here, so that no strategy can get them wrong:
//!
//! * Entries only inside the session's entry window, on a healthy feed, in a
//!   stock marked tradable, and after the risk manager has approved them.
//! * A strategy can only exit or move the stop of its own trade.
//! * Square-off runs from the clock, not from a stock's bars, so a stock
//!   whose feed has stopped is still closed.
//! * When the daily loss limit is reached, everything is closed.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod journal;

pub use journal::ClosedTrade;

use costs::{CostConfig, CostError, CostModel};
use indicators::{BankBuilder, IndicatorBank, IndicatorError, Registry};
use journal::Journal;
use oms::{EntryOrder, Oms, OmsEvent};
use risk::{Decision, RiskConfig, RiskError, RiskEvent, RiskManager};
use strategies::{Context, PositionView, StrategyError, StrategySet};
use trading_core::{
    Bar, BrokerOrderId, ClientOrderId, CoreError, ExecError, ExecutionReport, InstrumentId, Intent,
    MarketHours, Money, OrderSnapshot, PositionSnapshot, Price, Signal, StrategyId, Timestamp,
    Universe,
};

pub use oms::{Command, Mismatch};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Strategy(#[from] StrategyError),
    #[error(transparent)]
    Indicator(#[from] IndicatorError),
    #[error(transparent)]
    Risk(#[from] RiskError),
    #[error(transparent)]
    Cost(#[from] CostError),
    #[error(transparent)]
    Bar(#[from] CoreError),
    #[error(transparent)]
    Oms(#[from] oms::OmsError),
}

/// One strategy to run.
#[derive(Debug, Clone)]
pub struct StrategySpec {
    /// Name in [`strategies::build`].
    pub name: String,
    /// `None` trades every stock.
    pub stocks: Option<Vec<InstrumentId>>,
    /// Settings: a JSON object, with missing fields taking their defaults.
    pub params: serde_json::Value,
}

/// Everything needed to start an [`Engine`].
#[derive(Debug, Clone)]
pub struct EngineSetup {
    pub universe: Universe,
    pub hours: MarketHours,
    pub risk: RiskConfig,
    pub costs: CostConfig,
    pub strategies: Vec<StrategySpec>,
    /// Must differ between runs on the same day (see [`Oms::new`]).
    pub boot: u64,
}

/// Counts for reports and alerts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub entries_sent: u64,
    pub entries_outside_window: u64,
    pub entries_feed_down: u64,
    pub entries_not_tradable: u64,
    pub signals_for_a_trade_not_owned: u64,
    pub entries_refused_by_oms: u64,
    pub daily_loss_halts: u64,
    pub orders_failed: u64,
    pub stops_not_moved: u64,
}

/// See the [crate docs](crate).
#[derive(Debug)]
pub struct Engine {
    universe: Universe,
    hours: MarketHours,
    bank: IndicatorBank,
    strategies: StrategySet,
    risk: RiskManager,
    oms: Oms,
    journal: Journal,
    tradable: Vec<bool>,
    feed_ok: bool,
    last_close: Vec<Price>,
    day: i64,
    /// Close everything today, whatever the time.
    flatten: bool,
    signals: Vec<Signal>,
    stats: EngineStats,
}

impl Engine {
    /// Fails on any invalid setting, before any data flows.
    pub fn new(setup: EngineSetup) -> Result<Self, EngineError> {
        let n = setup.universe.len();
        let model = CostModel::new(&setup.costs)?;
        let mut builder = BankBuilder::new(Registry::builtin()).with_hours(setup.hours);
        let mut set = StrategySet::new(n);
        let all: Vec<InstrumentId> = setup.universe.iter().map(|i| i.id).collect();
        for (i, spec) in setup.strategies.into_iter().enumerate() {
            let id = StrategyId(u16::try_from(i).map_err(|_| StrategyError::Config {
                strategy: "engine",
                why: "too many strategies".into(),
            })?);
            let strategy = strategies::build(&spec.name, id, spec.params, &mut builder, n)?;
            set.add(strategy, spec.stocks.as_deref().unwrap_or(&all))?;
        }
        Ok(Self {
            hours: setup.hours,
            bank: builder.build(n)?,
            strategies: set,
            risk: RiskManager::new(&setup.risk, model.clone(), n)?,
            oms: Oms::new(setup.boot, n),
            journal: Journal::new(model, n),
            tradable: vec![true; n],
            feed_ok: true,
            last_close: vec![Price::ZERO; n],
            day: i64::MIN,
            flatten: false,
            signals: Vec::new(),
            stats: EngineStats::default(),
            universe: setup.universe,
        })
    }

    pub fn risk(&self) -> &RiskManager {
        &self.risk
    }

    /// The stock's intraday leverage from the broker's margin list.
    pub fn set_leverage(&mut self, id: InstrumentId, leverage: f64) -> Result<(), RiskError> {
        self.risk.set_leverage(id, leverage)
    }

    /// Whether the stock may be traded intraday today.
    pub fn set_tradable(&mut self, id: InstrumentId, tradable: bool) {
        if let Some(t) = self.tradable.get_mut(id.index()) {
            *t = tradable;
        }
    }

    /// Whether market data is flowing normally.
    pub fn set_feed_ok(&mut self, ok: bool) {
        self.feed_ok = ok;
    }

    pub fn stats(&self) -> EngineStats {
        self.stats
    }

    /// Each strategy's name and filter counts.
    pub fn filter_counts(&self) -> Vec<(&'static str, Vec<(&'static str, u64)>)> {
        self.strategies.filter_counts()
    }

    /// Trades finished since the last call.
    pub fn take_closed_trades(&mut self) -> Vec<ClosedTrade> {
        self.journal.take_closed()
    }

    pub fn take_commands(&mut self) -> Vec<Command> {
        self.oms.take_commands()
    }

    /// Stocks where our position differs from the broker's.
    pub fn reconcile(&self, broker: &[PositionSnapshot]) -> Vec<Mismatch> {
        self.oms.reconcile(broker)
    }

    /// Feeds indicators only, for building history before trading starts.
    pub fn warm_up(&mut self, bar: &Bar) -> Result<(), EngineError> {
        bar.validate()?;
        self.bank.update(bar)?;
        Ok(())
    }

    /// Rebuilds state after a restart, from the broker's positions and
    /// today's realized P&L after charges.
    pub fn restore(
        &mut self,
        positions: &[PositionSnapshot],
        realized_after_charges: Money,
        now: Timestamp,
    ) -> Result<(), EngineError> {
        self.roll_day(now);
        self.oms.restore(positions, now)?;
        let event = self.risk.restore(positions, realized_after_charges, now)?;
        self.on_risk_event(event, now);
        Ok(())
    }

    /// Kill switch: no new trades, and everything open is closed.
    pub fn stop_trading(&mut self, now: Timestamp) {
        self.risk.halt();
        self.flatten = true;
        self.oms.flatten_all(now);
    }

    fn roll_day(&mut self, ts: Timestamp) {
        let day = ts.ist_day();
        if day > self.day {
            self.day = day;
            self.flatten = false;
            self.oms.begin_day();
        }
    }

    fn on_risk_event(&mut self, event: Option<RiskEvent>, now: Timestamp) {
        if let Some(RiskEvent::DailyLossHit { .. }) = event {
            self.stats.daily_loss_halts += 1;
            self.flatten = true;
            self.oms.flatten_all(now);
        }
    }

    /// Passes what the order manager reported to the risk manager and the
    /// trade journal.
    fn pump_events(&mut self, now: Timestamp) {
        loop {
            let events = self.oms.take_events();
            if events.is_empty() {
                return;
            }
            for event in events {
                match event {
                    OmsEvent::Filled { fill, strategy } => {
                        self.journal.on_fill(&fill, strategy);
                        let qty = self.oms.position(fill.instrument).map_or(0, |p| p.qty);
                        self.journal.after_fill(fill.instrument, qty);
                        let risk_event = self.risk.on_fill(&fill);
                        self.on_risk_event(risk_event, now);
                    }
                    OmsEvent::EntryFinished { instrument, .. } => self.risk.release(instrument),
                    OmsEvent::OrderFailed { .. } => self.stats.orders_failed += 1,
                    OmsEvent::StopNotMoved(_) => self.stats.stops_not_moved += 1,
                    OmsEvent::UnknownOrderReport(_) => {}
                }
            }
        }
    }

    /// A report from the broker. `now` is the current time.
    pub fn on_report(&mut self, report: &ExecutionReport, now: Timestamp) {
        self.roll_day(report.ts);
        self.oms.on_report(report, now);
        self.pump_events(now);
    }

    pub fn on_submit_result(
        &mut self,
        id: ClientOrderId,
        result: &Result<BrokerOrderId, ExecError>,
        now: Timestamp,
    ) {
        self.oms.on_submit_result(id, result, now);
        self.pump_events(now);
    }

    pub fn on_cancel_result(
        &mut self,
        id: ClientOrderId,
        result: &Result<(), ExecError>,
        now: Timestamp,
    ) {
        self.oms.on_cancel_result(id, result);
        self.pump_events(now);
    }

    pub fn on_modify_result(
        &mut self,
        id: ClientOrderId,
        result: &Result<(), ExecError>,
        now: Timestamp,
    ) {
        self.oms.on_modify_result(id, result);
        self.pump_events(now);
    }

    pub fn on_lookup_result(
        &mut self,
        id: ClientOrderId,
        result: &Result<Option<OrderSnapshot>, ExecError>,
        now: Timestamp,
    ) {
        self.oms.on_lookup_result(id, result, now);
        self.pump_events(now);
    }

    /// A completed 1-minute bar.
    pub fn on_bar(&mut self, bar: &Bar) -> Result<(), EngineError> {
        bar.validate()?;
        self.roll_day(bar.ts);
        let session = self.bank.update(bar)?;
        let close = bar.ts.plus_minutes(1);
        if let Some(last) = self.last_close.get_mut(bar.instrument.index()) {
            *last = bar.close;
        }
        let event = self.risk.on_mark(bar.instrument, bar.close, close);
        self.on_risk_event(event, close);

        let Some(instrument) = self.universe.get(bar.instrument) else {
            return Ok(());
        };
        let mut signals = std::mem::take(&mut self.signals);
        signals.clear();
        let tradable = self
            .tradable
            .get(bar.instrument.index())
            .copied()
            .unwrap_or(false);
        let (bank, oms, hours, feed_ok) = (&self.bank, &self.oms, self.hours, self.feed_ok);
        self.strategies.on_bar(
            bar,
            |strategy| {
                context(
                    bank, oms, strategy, instrument, session, hours, feed_ok, tradable,
                )
            },
            &mut signals,
        );
        for signal in &signals {
            self.process(signal, close);
        }
        self.signals = signals;
        self.pump_events(close);
        Ok(())
    }

    /// Every stock's bar for the minute ending at `close` is in (or its
    /// deadline has passed).
    pub fn on_minute_end(&mut self, close: Timestamp) {
        self.roll_day(close);
        let mut signals = std::mem::take(&mut self.signals);
        signals.clear();
        self.strategies.on_minute_end(close, &mut signals);
        for signal in &signals {
            self.process(signal, close);
        }
        self.signals = signals;
        self.on_clock(close);
        self.pump_events(close);
    }

    /// Time-driven work. Runs from each minute end; a live driver should
    /// also call it on a timer in case no bars arrive at all.
    pub fn on_clock(&mut self, now: Timestamp) {
        self.roll_day(now);
        self.oms.tick();
        if self.flatten || self.hours.must_square_off(now) {
            self.oms.flatten_all(now);
        }
        self.pump_events(now);
    }

    fn process(&mut self, signal: &Signal, now: Timestamp) {
        let i = signal.instrument;
        let Some(instrument) = self.universe.get(i) else {
            return;
        };
        let reference = self
            .last_close
            .get(i.index())
            .copied()
            .unwrap_or(Price::ZERO);
        match signal.intent {
            Intent::EnterLong | Intent::EnterShort => {
                if !self.hours.entries_allowed(signal.ts) {
                    self.stats.entries_outside_window += 1;
                    return;
                }
                if !self.feed_ok {
                    self.stats.entries_feed_down += 1;
                    return;
                }
                if !self.tradable.get(i.index()).copied().unwrap_or(false) {
                    self.stats.entries_not_tradable += 1;
                    return;
                }
                let Decision::Enter(a) = self.risk.evaluate(signal, instrument, reference) else {
                    return;
                };
                let order = EntryOrder {
                    strategy: signal.strategy,
                    side: a.side,
                    qty: a.qty,
                    limit: signal.entry,
                    stop: a.stop,
                    target: Some(a.target),
                };
                match self.oms.enter(instrument, &order, a.entry, now) {
                    Ok(_) => {
                        self.stats.entries_sent += 1;
                        self.journal.on_entry(i, signal, &a);
                    }
                    Err(_) => {
                        self.stats.entries_refused_by_oms += 1;
                        self.risk.release(i);
                    }
                }
            }
            Intent::Exit => {
                if self.oms.owner(i) != Some(signal.strategy) {
                    self.stats.signals_for_a_trade_not_owned += 1;
                    return;
                }
                self.journal.on_exit_signal(i, signal.reason);
                self.oms.exit(i, now);
            }
            Intent::AdjustStop => {
                if self.oms.owner(i) != Some(signal.strategy) {
                    self.stats.signals_for_a_trade_not_owned += 1;
                    return;
                }
                if matches!(
                    self.risk.evaluate(signal, instrument, reference),
                    Decision::Allow
                ) && let Some(stop) = signal.stop
                {
                    self.oms.adjust_stop(i, stop);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn context<'a>(
    bank: &'a IndicatorBank,
    oms: &Oms,
    strategy: StrategyId,
    instrument: &'a trading_core::Instrument,
    session: indicators::BarContext,
    hours: MarketHours,
    feed_ok: bool,
    tradable: bool,
) -> Context<'a> {
    let (qty, avg_price, realized) = oms.view(strategy, instrument.id);
    Context {
        instrument,
        bank,
        session,
        hours,
        feed_ok,
        tradable,
        position: PositionView {
            qty,
            avg_price,
            realized,
        },
    }
}

#[cfg(test)]
mod tests;
