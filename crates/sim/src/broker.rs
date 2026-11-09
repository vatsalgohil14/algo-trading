//! A simulated broker: fills orders from 1-minute bars, deliberately on the
//! pessimistic side, and can inject the failures a real broker produces.
//!
//! Fill rules:
//!
//! * An order works from the first bar that starts at or after the moment it
//!   was placed, so a decision can never trade on its own bar.
//! * **Limit entries** fill if the bar opens at or through the limit, or
//!   trades *beyond* it. Merely touching the limit is not a fill, since we
//!   may be at the back of the queue, so entries in a runaway move are missed
//!   as they would be in practice.
//! * A fill takes at most `max_participation` of the bar's volume; the rest
//!   waits for later bars.
//! * **Stops** fill at the worse of the stop and the open, plus slippage.
//! * **Targets** need a trade beyond the level and fill at it, or at the
//!   open if it gapped through.
//! * If a bar reaches both, the **stop is assumed to come first**. On the bar
//!   an entry fills, its stop is live but its target is not.

use std::collections::HashMap;
use std::sync::Arc;

use costs::Slippage;
use trading_core::{
    Bar, BrokerOrderId, ClientOrderId, ExecError, ExecutionReport, Fill, InstrumentId, Money,
    OrderChange, OrderEvent, OrderKind, OrderRequest, OrderSnapshot, OrderStatus, Position,
    PositionSnapshot, Price, Side, Timestamp,
};

use crate::rng::Rng;

/// How often things go wrong, as probabilities from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Faults {
    /// The broker refuses the order.
    pub reject: f64,
    /// The request times out and the order was not placed.
    pub unknown_not_placed: f64,
    /// The request times out but the order *was* placed.
    pub unknown_placed: f64,
    /// A fill is reported twice.
    pub duplicate_fill: f64,
    /// A cancel request fails, and the order stays live.
    pub cancel_fails: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimBrokerConfig {
    pub seed: u64,
    pub slippage: Slippage,
    /// Largest share of a bar's volume one order can take.
    pub max_participation: f64,
    pub faults: Faults,
}

impl Default for SimBrokerConfig {
    fn default() -> Self {
        Self {
            seed: 1,
            slippage: Slippage::default(),
            max_participation: 0.10,
            faults: Faults::default(),
        }
    }
}

#[derive(Debug, Clone)]
struct SimOrder {
    request: OrderRequest,
    broker_id: BrokerOrderId,
    active_from: Timestamp,
    filled: u32,
    value: i128,
    /// Shares of `filled` already closed by the stop or target.
    closed: u32,
    stop: Option<Price>,
    target: Option<Price>,
    status: OrderStatus,
}

/// See the [module docs](self).
#[derive(Debug)]
pub struct SimBroker {
    config: SimBrokerConfig,
    rng: Rng,
    ticks: Vec<Price>,
    orders: Vec<SimOrder>,
    by_id: HashMap<ClientOrderId, usize>,
    /// Indexes of live orders per stock.
    live: Vec<Vec<usize>>,
    positions: Vec<Position>,
    last_price: Vec<Price>,
    reports: Vec<ExecutionReport>,
    next_trade: u64,
    auto_square_offs: u64,
}

impl SimBroker {
    /// `ticks` holds the tick size of each stock, by instrument id.
    pub fn new(config: SimBrokerConfig, ticks: Vec<Price>) -> Self {
        let n = ticks.len();
        Self {
            rng: Rng::new(config.seed),
            config,
            ticks,
            orders: Vec::new(),
            by_id: HashMap::new(),
            live: vec![Vec::new(); n],
            positions: (0..n)
                .map(|i| Position::flat(InstrumentId(i as u32)))
                .collect(),
            last_price: vec![Price::ZERO; n],
            reports: Vec::new(),
            next_trade: 0,
            auto_square_offs: 0,
        }
    }

    /// Reports produced since the last call, oldest first.
    pub fn take_reports(&mut self) -> Vec<ExecutionReport> {
        std::mem::take(&mut self.reports)
    }

    /// Positions the broker had to close itself at the end of a day. A
    /// correct system never leaves one.
    pub fn auto_square_offs(&self) -> u64 {
        self.auto_square_offs
    }

    /// Realized P&L before charges over the whole run, from the broker's
    /// own books, for cross-checking the system's.
    pub fn total_realized(&self) -> Money {
        self.positions.iter().map(|p| p.realized).sum()
    }

    fn roll(&mut self, chance: f64) -> bool {
        chance > 0.0 && self.rng.chance(chance)
    }

    fn report(&mut self, index: usize, ts: Timestamp, event: OrderEvent) {
        if let Some(o) = self.orders.get(index) {
            self.reports.push(ExecutionReport {
                client_id: o.request.client_id,
                broker_order_id: Some(o.broker_id.clone()),
                ts,
                event,
            });
        }
    }

    /// Places an order, effective from the first bar starting at or after
    /// `now`.
    pub fn submit(
        &mut self,
        request: &OrderRequest,
        now: Timestamp,
    ) -> Result<BrokerOrderId, ExecError> {
        let faults = self.config.faults;
        if self.roll(faults.reject) {
            return Err(ExecError::Rejected("simulated rejection".into()));
        }
        if self.roll(faults.unknown_not_placed) {
            return Err(ExecError::OutcomeUnknown("simulated timeout".into()));
        }
        if !matches!(request.kind, OrderKind::Market | OrderKind::Limit { .. }) {
            return Err(ExecError::Rejected("order type not simulated".into()));
        }
        let Some(live) = self.live.get_mut(request.instrument.index()) else {
            return Err(ExecError::Rejected("unknown instrument".into()));
        };
        let index = self.orders.len();
        live.push(index);
        let broker_id: BrokerOrderId = Arc::from(format!("sim{index}"));
        self.orders.push(SimOrder {
            request: request.clone(),
            broker_id: broker_id.clone(),
            active_from: now,
            filled: 0,
            value: 0,
            closed: 0,
            stop: request.bracket.map(|b| b.stop),
            target: request.bracket.and_then(|b| b.target),
            status: OrderStatus::Open,
        });
        self.by_id.insert(request.client_id, index);
        self.report(index, now, OrderEvent::Accepted);
        if self.roll(faults.unknown_placed) {
            return Err(ExecError::OutcomeUnknown("simulated timeout".into()));
        }
        Ok(broker_id)
    }

    fn find_live(&self, id: ClientOrderId) -> Result<usize, ExecError> {
        let index = *self.by_id.get(&id).ok_or(ExecError::UnknownOrder(id))?;
        match self.orders.get(index) {
            Some(o) if !o.status.is_terminal() => Ok(index),
            _ => Err(ExecError::Rejected("order already finished".into())),
        }
    }

    /// Cancels a working entry and any stop and target attached to it. An
    /// open position stays open.
    pub fn cancel(&mut self, id: ClientOrderId, now: Timestamp) -> Result<(), ExecError> {
        let index = self.find_live(id)?;
        if self.roll(self.config.faults.cancel_fails) {
            return Err(ExecError::OutcomeUnknown("simulated timeout".into()));
        }
        self.finish(index, OrderStatus::Cancelled);
        self.report(index, now, OrderEvent::Cancelled);
        Ok(())
    }

    /// Moves the stop attached to an order.
    pub fn modify(
        &mut self,
        id: ClientOrderId,
        change: OrderChange,
        now: Timestamp,
    ) -> Result<(), ExecError> {
        let index = self.find_live(id)?;
        let order = self
            .orders
            .get_mut(index)
            .ok_or(ExecError::UnknownOrder(id))?;
        match (change.trigger, order.stop) {
            (Some(trigger), Some(_)) => order.stop = Some(trigger),
            _ => return Err(ExecError::Rejected("only stops can be moved".into())),
        }
        self.report(index, now, OrderEvent::Modified);
        Ok(())
    }

    /// Looks an order up by our id.
    pub fn order_status(&self, id: ClientOrderId) -> Option<OrderSnapshot> {
        let o = self.orders.get(*self.by_id.get(&id)?)?;
        let avg = (o.filled > 0).then(|| {
            let paise = o.value / i128::from(o.filled);
            Price::from_paise(i64::try_from(paise).unwrap_or(i64::MAX))
        });
        Some(OrderSnapshot {
            client_id: id,
            broker_order_id: o.broker_id.clone(),
            instrument: o.request.instrument,
            status: o.status,
            filled_qty: o.filled,
            avg_fill_price: avg,
        })
    }

    /// Net positions.
    pub fn positions(&self) -> Vec<PositionSnapshot> {
        self.positions
            .iter()
            .filter(|p| p.qty != 0)
            .map(|p| PositionSnapshot {
                instrument: p.instrument,
                qty: p.qty,
                avg_price: p.avg_price(),
            })
            .collect()
    }

    fn finish(&mut self, index: usize, status: OrderStatus) {
        let Some(order) = self.orders.get_mut(index) else {
            return;
        };
        order.status = status;
        let stock = order.request.instrument.index();
        if let Some(live) = self.live.get_mut(stock) {
            live.retain(|i| *i != index);
        }
    }

    fn fill(&mut self, index: usize, side: Side, qty: u32, price: Price, ts: Timestamp) {
        let Some(order) = self.orders.get(index) else {
            return;
        };
        let client_id = order.request.client_id;
        let instrument = order.request.instrument;
        self.next_trade += 1;
        let fill = Fill {
            client_id,
            instrument,
            side,
            qty,
            price,
            ts,
            trade_id: Arc::from(format!("t{}", self.next_trade)),
        };
        if let Some(position) = self.positions.get_mut(instrument.index()) {
            position.apply(&fill);
        }
        let twice = self.roll(self.config.faults.duplicate_fill);
        if twice {
            self.report(index, ts, OrderEvent::Fill(fill.clone()));
        }
        self.report(index, ts, OrderEvent::Fill(fill));
    }

    /// Plays one bar against the working orders of its stock.
    pub fn on_bar(&mut self, bar: &Bar) {
        let stock = bar.instrument.index();
        if let Some(last) = self.last_price.get_mut(stock) {
            *last = bar.close;
        }
        let Some(live) = self.live.get(stock) else {
            return;
        };
        for index in live.clone() {
            self.play(index, bar);
        }
    }

    fn play(&mut self, index: usize, bar: &Bar) {
        let Some(order) = self.orders.get(index) else {
            return;
        };
        if bar.ts < order.active_from {
            return;
        }
        let side = order.request.side;
        let remaining = order.request.qty.saturating_sub(order.filled);
        let bracketed = order.request.bracket.is_some();
        let tick = self
            .ticks
            .get(bar.instrument.index())
            .copied()
            .unwrap_or(Price::from_paise(5));

        let mut filled_now = false;
        if remaining > 0
            && let Some((qty, price)) = self.entry_fill(order, remaining, bar, tick)
        {
            if let Some(o) = self.orders.get_mut(index) {
                o.filled += qty;
                o.value += i128::from(price.paise()) * i128::from(qty);
                if o.status == OrderStatus::Open {
                    o.status = OrderStatus::PartiallyFilled;
                }
            }
            self.fill(index, side, qty, price, bar.ts);
            filled_now = true;
        }

        let Some(order) = self.orders.get(index) else {
            return;
        };
        let complete = order.filled >= order.request.qty;
        if !bracketed {
            if complete {
                self.finish(index, OrderStatus::Filled);
            }
            return;
        }

        let open_qty = order.filled.saturating_sub(order.closed);
        if open_qty == 0 {
            return;
        }
        let Some(price) = self.leg_fill(order, bar, tick, filled_now) else {
            return;
        };
        if let Some(o) = self.orders.get_mut(index) {
            o.closed = o.filled;
        }
        self.fill(index, side.opposite(), open_qty, price, bar.ts);
        // Once a leg has fired the rest of the entry no longer applies.
        self.finish(index, OrderStatus::Filled);
        if !complete {
            self.report(index, bar.ts, OrderEvent::Cancelled);
        }
    }

    fn entry_fill(
        &self,
        order: &SimOrder,
        remaining: u32,
        bar: &Bar,
        tick: Price,
    ) -> Option<(u32, Price)> {
        let side = order.request.side;
        let slipped = self.config.slippage.apply(side, bar.open, tick);
        let price = match order.request.kind {
            OrderKind::Limit { price: limit } => match side {
                Side::Buy if bar.open <= limit => slipped.min(limit),
                Side::Buy if bar.low < limit => limit,
                Side::Sell if bar.open >= limit => slipped.max(limit),
                Side::Sell if bar.high > limit => limit,
                _ => return None,
            },
            _ => slipped,
        };
        // Exits must get out, so only entries are limited by volume.
        let qty = if order.request.bracket.is_some() {
            let cap = (bar.volume as f64 * self.config.max_participation).floor() as u32;
            remaining.min(cap)
        } else {
            remaining
        };
        (qty > 0).then_some((qty, price))
    }

    /// The price at which the stop or target closes the trade on this bar.
    fn leg_fill(
        &self,
        order: &SimOrder,
        bar: &Bar,
        tick: Price,
        entered_this_bar: bool,
    ) -> Option<Price> {
        let long = order.request.side == Side::Buy;
        let close_side = order.request.side.opposite();
        if let Some(stop) = order.stop {
            let hit = if long {
                bar.low <= stop
            } else {
                bar.high >= stop
            };
            if hit {
                let base = if entered_this_bar {
                    stop
                } else if long {
                    bar.open.min(stop)
                } else {
                    bar.open.max(stop)
                };
                return Some(self.config.slippage.apply(close_side, base, tick));
            }
        }
        if entered_this_bar {
            return None;
        }
        let target = order.target?;
        if long && bar.high > target {
            Some(bar.open.max(target))
        } else if !long && bar.low < target {
            Some(bar.open.min(target))
        } else {
            None
        }
    }

    /// Ends the trading day: working orders expire, and anything still open
    /// is closed at the last price, as a broker's auto square-off would.
    pub fn end_of_day(&mut self, ts: Timestamp) {
        let indexes: Vec<usize> = self.live.iter().flatten().copied().collect();
        for index in indexes {
            self.finish(index, OrderStatus::Expired);
            self.report(index, ts, OrderEvent::Expired);
        }
        for stock in 0..self.positions.len() {
            let Some(position) = self.positions.get_mut(stock) else {
                continue;
            };
            if position.qty == 0 {
                continue;
            }
            let qty = u32::try_from(position.qty.unsigned_abs()).unwrap_or(u32::MAX);
            let side = if position.qty > 0 {
                Side::Sell
            } else {
                Side::Buy
            };
            let price = self.last_price.get(stock).copied().unwrap_or(Price::ZERO);
            self.next_trade += 1;
            let Ok(client_id) = ClientOrderId::new("autosquareoff") else {
                continue;
            };
            let fill = Fill {
                client_id,
                instrument: InstrumentId(stock as u32),
                side,
                qty,
                price,
                ts,
                trade_id: Arc::from(format!("t{}", self.next_trade)),
            };
            position.apply(&fill);
            self.auto_square_offs += 1;
            self.reports.push(ExecutionReport {
                client_id,
                broker_order_id: None,
                ts,
                event: OrderEvent::Fill(fill),
            });
        }
    }
}

#[cfg(test)]
mod tests;
