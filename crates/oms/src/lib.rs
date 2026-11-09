//! Order manager: knows every order and position, and decides what to send.
//!
//! It does no I/O. Calls go in ([`Oms::enter`], [`Oms::exit`],
//! [`Oms::on_report`], ...) and [`Command`]s come out for a driver to carry
//! out, so the same logic runs against the simulator and against a broker,
//! and every path can be tested without a network.
//!
//! Safety rules:
//!
//! * **Never blindly retry.** After [`ExecError::OutcomeUnknown`] the order
//!   may exist, so it is looked up by its id first, and the stock stays busy
//!   until that is settled.
//! * **Every fill counts once**, however often the broker reports it
//!   (matched by trade id). A fill for an order we do not know is still
//!   applied, because the position is real.
//! * **An exit is one exit.** Repeat requests are ignored while one is in
//!   flight. A protected position has its stop and target cancelled first,
//!   and only once that is confirmed is the closing order sent for the
//!   quantity held then, so the two can never both sell.
//! * [`Oms::reconcile`] compares our positions with the broker's. The broker
//!   is the source of truth.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use trading_core::{
    Bracket, BrokerOrderId, ClientOrderId, ExecError, ExecutionReport, Fill, Instrument,
    InstrumentId, Money, OrderChange, OrderEvent, OrderKind, OrderRequest, OrderSeq, OrderSnapshot,
    OrderStatus, Position, PositionSnapshot, Price, Product, Side, StrategyId, Timestamp,
};

/// Owner of orders no strategy claims, such as closing a restored position.
const SYSTEM: StrategyId = StrategyId(1295);

/// Something for the driver to do at the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Submit(OrderRequest),
    Modify {
        id: ClientOrderId,
        change: OrderChange,
    },
    Cancel(ClientOrderId),
    /// Find out what happened to an order.
    Lookup(ClientOrderId),
}

/// Something the rest of the system should know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmsEvent {
    /// A new execution, already applied to the position.
    Filled {
        fill: Fill,
        strategy: Option<StrategyId>,
    },
    /// An entry order is finished: fully filled, cancelled, rejected, or
    /// never placed.
    EntryFinished {
        instrument: InstrumentId,
        strategy: StrategyId,
        filled: u32,
    },
    /// The broker refused an order, or it could not be sent.
    OrderFailed {
        id: ClientOrderId,
        instrument: InstrumentId,
        reason: Arc<str>,
    },
    /// A report for an order we have no record of.
    UnknownOrderReport(ClientOrderId),
    /// A stop move did not reach the broker; the old stop still stands.
    StopNotMoved(InstrumentId),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OmsError {
    #[error("stock {0} already has an order or position")]
    Busy(InstrumentId),
    #[error("instrument {0} is outside the universe")]
    UnknownInstrument(InstrumentId),
    #[error("order is invalid: {0}")]
    Invalid(String),
    #[error("order ids are exhausted")]
    OutOfIds,
}

/// An approved entry, as limits and quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryOrder {
    pub strategy: StrategyId,
    pub side: Side,
    pub qty: u32,
    /// `None` enters at market.
    pub limit: Option<Price>,
    pub stop: Price,
    pub target: Option<Price>,
}

/// Our position differs from the broker's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mismatch {
    pub instrument: InstrumentId,
    pub ours: i64,
    pub broker: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Entry,
    Exit,
}

#[derive(Debug, Clone, Copy)]
struct Order {
    instrument: InstrumentId,
    strategy: StrategyId,
    side: Side,
    role: Role,
    qty: u32,
    filled: u32,
    done: bool,
    entry_reported: bool,
}

#[derive(Debug, Clone)]
struct Stock {
    position: Position,
    owner: Option<StrategyId>,
    /// The entry order, and with it the stop and target at the broker.
    entry: Option<ClientOrderId>,
    exit: Option<ClientOrderId>,
    /// Close the position once the entry's cancellation is confirmed.
    exit_after_cancel: bool,
    realized: Vec<(StrategyId, Money)>,
}

impl Stock {
    fn is_busy(&self) -> bool {
        self.position.qty != 0 || self.entry.is_some() || self.exit.is_some()
    }
}

/// See the [crate docs](crate).
#[derive(Debug)]
pub struct Oms {
    boot: u64,
    seq: OrderSeq,
    stocks: Vec<Stock>,
    orders: HashMap<ClientOrderId, Order>,
    seen_trades: HashSet<Arc<str>>,
    /// Orders whose fate is not known yet; asked about on every tick.
    unresolved: BTreeSet<String>,
    commands: Vec<Command>,
    events: Vec<OmsEvent>,
}

impl Oms {
    /// `boot` must differ between runs on the same day (e.g. the start time
    /// in seconds), so that order ids stay unique across restarts.
    pub fn new(boot: u64, instruments: usize) -> Self {
        let stocks = (0..instruments)
            .map(|i| Stock {
                position: Position::flat(InstrumentId(i as u32)),
                owner: None,
                entry: None,
                exit: None,
                exit_after_cancel: false,
                realized: Vec::new(),
            })
            .collect();
        Self {
            boot,
            seq: OrderSeq::default(),
            stocks,
            orders: HashMap::new(),
            seen_trades: HashSet::new(),
            unresolved: BTreeSet::new(),
            commands: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Commands waiting to be carried out, oldest first.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    pub fn take_events(&mut self) -> Vec<OmsEvent> {
        std::mem::take(&mut self.events)
    }

    /// Forgets the finished orders and today's realized P&L.
    pub fn begin_day(&mut self) {
        self.orders.retain(|_, o| !o.done);
        self.seen_trades.clear();
        for stock in &mut self.stocks {
            stock.realized.clear();
        }
    }

    /// The net position in a stock.
    pub fn position(&self, instrument: InstrumentId) -> Option<&Position> {
        self.stocks.get(instrument.index()).map(|s| &s.position)
    }

    /// Who owns the position or working order in a stock.
    pub fn owner(&self, instrument: InstrumentId) -> Option<StrategyId> {
        self.stocks.get(instrument.index())?.owner
    }

    /// `strategy`'s shares, average price and realized P&L today in a stock.
    pub fn view(
        &self,
        strategy: StrategyId,
        instrument: InstrumentId,
    ) -> (i64, Option<Price>, Money) {
        let Some(stock) = self.stocks.get(instrument.index()) else {
            return (0, None, Money::ZERO);
        };
        let realized = stock
            .realized
            .iter()
            .find(|(s, _)| *s == strategy)
            .map_or(Money::ZERO, |(_, m)| *m);
        if stock.owner == Some(strategy) && stock.position.qty != 0 {
            (stock.position.qty, stock.position.avg_price(), realized)
        } else {
            (0, None, realized)
        }
    }

    pub fn is_busy(&self, instrument: InstrumentId) -> bool {
        self.stocks
            .get(instrument.index())
            .is_some_and(Stock::is_busy)
    }

    /// Stocks with a position or a working order.
    pub fn busy_stocks(&self) -> impl Iterator<Item = InstrumentId> + '_ {
        self.stocks
            .iter()
            .filter(|s| s.is_busy())
            .map(|s| s.position.instrument)
    }

    fn next_id(&mut self, strategy: StrategyId) -> Result<ClientOrderId, OmsError> {
        let seq = self.seq.advance().ok_or(OmsError::OutOfIds)?;
        Ok(ClientOrderId::compose(self.boot, strategy, seq))
    }

    /// Sends an entry with its stop and target attached (a super order).
    pub fn enter(
        &mut self,
        instrument: &Instrument,
        order: &EntryOrder,
        reference: Price,
        now: Timestamp,
    ) -> Result<ClientOrderId, OmsError> {
        let stock = self
            .stocks
            .get(instrument.id.index())
            .ok_or(OmsError::UnknownInstrument(instrument.id))?;
        if stock.is_busy() {
            return Err(OmsError::Busy(instrument.id));
        }
        let client_id = self.next_id(order.strategy)?;
        let request = OrderRequest {
            client_id,
            strategy: order.strategy,
            instrument: instrument.id,
            side: order.side,
            qty: order.qty,
            kind: order
                .limit
                .map_or(OrderKind::Market, |price| OrderKind::Limit { price }),
            product: Product::Intraday,
            bracket: Some(Bracket {
                stop: order.stop,
                target: order.target,
            }),
            created: now,
        };
        request
            .validate(instrument, reference)
            .map_err(|e| OmsError::Invalid(e.to_string()))?;
        self.orders.insert(
            client_id,
            Order {
                instrument: instrument.id,
                strategy: order.strategy,
                side: order.side,
                role: Role::Entry,
                qty: order.qty,
                filled: 0,
                done: false,
                entry_reported: false,
            },
        );
        if let Some(stock) = self.stocks.get_mut(instrument.id.index()) {
            stock.owner = Some(order.strategy);
            stock.entry = Some(client_id);
        }
        self.commands.push(Command::Submit(request));
        Ok(client_id)
    }

    /// Closes the position in a stock and cancels its working orders.
    /// Safe to repeat; does nothing if there is nothing to do.
    pub fn exit(&mut self, instrument: InstrumentId, now: Timestamp) {
        let Some(stock) = self.stocks.get_mut(instrument.index()) else {
            return;
        };
        if stock.exit.is_some() || stock.exit_after_cancel {
            return;
        }
        if let Some(entry) = stock.entry {
            stock.exit_after_cancel = true;
            self.commands.push(Command::Cancel(entry));
            return;
        }
        if stock.position.qty != 0 {
            self.send_exit(instrument, now);
        }
    }

    /// Exits every stock with a position or working order.
    pub fn flatten_all(&mut self, now: Timestamp) {
        let busy: Vec<_> = self.busy_stocks().collect();
        for instrument in busy {
            self.exit(instrument, now);
        }
    }

    fn send_exit(&mut self, instrument: InstrumentId, now: Timestamp) {
        let Some(stock) = self.stocks.get(instrument.index()) else {
            return;
        };
        let qty = stock.position.qty;
        let strategy = stock.owner.unwrap_or(SYSTEM);
        let (Ok(shares), Ok(client_id)) =
            (u32::try_from(qty.unsigned_abs()), self.next_id(strategy))
        else {
            return;
        };
        let side = if qty > 0 { Side::Sell } else { Side::Buy };
        self.orders.insert(
            client_id,
            Order {
                instrument,
                strategy,
                side,
                role: Role::Exit,
                qty: shares,
                filled: 0,
                done: false,
                entry_reported: true,
            },
        );
        if let Some(stock) = self.stocks.get_mut(instrument.index()) {
            stock.exit = Some(client_id);
        }
        self.commands.push(Command::Submit(OrderRequest {
            client_id,
            strategy,
            instrument,
            side,
            qty: shares,
            kind: OrderKind::Market,
            product: Product::Intraday,
            bracket: None,
            created: now,
        }));
    }

    /// Moves the stop attached to a stock's entry order.
    pub fn adjust_stop(&mut self, instrument: InstrumentId, stop: Price) {
        if let Some(id) = self.stocks.get(instrument.index()).and_then(|s| s.entry) {
            self.commands.push(Command::Modify {
                id,
                change: OrderChange {
                    trigger: Some(stop),
                    ..OrderChange::default()
                },
            });
        }
    }

    /// Asks again about orders whose outcome is still unknown. Call it
    /// about once a minute.
    pub fn tick(&mut self) {
        let ids: Vec<ClientOrderId> = self
            .unresolved
            .iter()
            .filter_map(|id| ClientOrderId::new(id).ok())
            .collect();
        self.commands.extend(ids.into_iter().map(Command::Lookup));
    }

    fn ask_about(&mut self, id: ClientOrderId) {
        if self.unresolved.insert(id.as_str().to_owned()) {
            self.commands.push(Command::Lookup(id));
        }
    }

    /// The answer to a [`Command::Submit`].
    pub fn on_submit_result(
        &mut self,
        id: ClientOrderId,
        result: &Result<BrokerOrderId, ExecError>,
        now: Timestamp,
    ) {
        match result {
            Ok(_) => {}
            Err(ExecError::OutcomeUnknown(_)) => self.ask_about(id),
            Err(e) => self.failed(id, e.to_string().into(), now),
        }
    }

    /// The answer to a [`Command::Cancel`].
    pub fn on_cancel_result(&mut self, id: ClientOrderId, result: &Result<(), ExecError>) {
        if result.is_err() {
            // Already finished, or unknown: find out which.
            self.ask_about(id);
        }
    }

    /// The answer to a [`Command::Modify`].
    pub fn on_modify_result(&mut self, id: ClientOrderId, result: &Result<(), ExecError>) {
        if result.is_err()
            && let Some(order) = self.orders.get(&id)
        {
            self.events.push(OmsEvent::StopNotMoved(order.instrument));
        }
    }

    /// The answer to a [`Command::Lookup`]. On error the order stays
    /// unresolved and is asked about again on the next [`tick`](Self::tick).
    pub fn on_lookup_result(
        &mut self,
        id: ClientOrderId,
        result: &Result<Option<OrderSnapshot>, ExecError>,
        now: Timestamp,
    ) {
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(_) => return,
        };
        self.unresolved.remove(id.as_str());
        match snapshot {
            None => self.failed(id, "never reached the broker".into(), now),
            Some(s) => match s.status {
                OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                    self.ended(id, now);
                }
                OrderStatus::Filled => {
                    // The entry and its legs are gone; fills arrive as reports.
                    if let Some(order) = self.orders.get(&id).copied()
                        && order.role == Role::Entry
                        && self.exit_is_waiting(order.instrument)
                    {
                        self.ended(id, now);
                    }
                }
                OrderStatus::PendingNew | OrderStatus::Open | OrderStatus::PartiallyFilled => {
                    // Still live, so the cancel did not work. The next
                    // exit request or clock tick tries again; retrying here
                    // could loop for as long as the broker keeps failing.
                    if let Some(order) = self.orders.get(&id).copied()
                        && let Some(stock) = self.stocks.get_mut(order.instrument.index())
                    {
                        stock.exit_after_cancel = false;
                    }
                }
            },
        }
    }

    fn exit_is_waiting(&self, instrument: InstrumentId) -> bool {
        self.stocks
            .get(instrument.index())
            .is_some_and(|s| s.exit_after_cancel)
    }

    fn failed(&mut self, id: ClientOrderId, reason: Arc<str>, now: Timestamp) {
        if let Some(order) = self.orders.get(&id) {
            self.events.push(OmsEvent::OrderFailed {
                id,
                instrument: order.instrument,
                reason,
            });
        }
        self.ended(id, now);
    }

    /// An order will never do anything more.
    fn ended(&mut self, id: ClientOrderId, now: Timestamp) {
        let Some(order) = self.orders.get_mut(&id) else {
            return;
        };
        if order.done {
            return;
        }
        order.done = true;
        let order = *order;
        self.report_entry_finished(id);
        let Some(stock) = self.stocks.get_mut(order.instrument.index()) else {
            return;
        };
        match order.role {
            Role::Exit => {
                if stock.exit == Some(id) {
                    stock.exit = None;
                }
            }
            Role::Entry => {
                if stock.entry == Some(id) {
                    stock.entry = None;
                }
                let close = std::mem::take(&mut stock.exit_after_cancel);
                if close && stock.position.qty != 0 && stock.exit.is_none() {
                    self.send_exit(order.instrument, now);
                }
            }
        }
        self.release_owner(order.instrument);
    }

    fn report_entry_finished(&mut self, id: ClientOrderId) {
        if let Some(order) = self.orders.get_mut(&id)
            && order.role == Role::Entry
            && !order.entry_reported
        {
            order.entry_reported = true;
            self.events.push(OmsEvent::EntryFinished {
                instrument: order.instrument,
                strategy: order.strategy,
                filled: order.filled,
            });
        }
    }

    fn release_owner(&mut self, instrument: InstrumentId) {
        if let Some(stock) = self.stocks.get_mut(instrument.index())
            && !stock.is_busy()
        {
            stock.owner = None;
        }
    }

    /// A report from the broker.
    pub fn on_report(&mut self, report: &ExecutionReport, now: Timestamp) {
        let id = report.client_id;
        match &report.event {
            OrderEvent::Accepted | OrderEvent::Modified => {}
            OrderEvent::Rejected { reason } => self.failed(id, reason.clone(), now),
            OrderEvent::Cancelled | OrderEvent::Expired => self.ended(id, now),
            OrderEvent::Fill(fill) => self.on_fill(id, fill),
        }
        if !self.orders.contains_key(&id) {
            self.events.push(OmsEvent::UnknownOrderReport(id));
        }
    }

    fn on_fill(&mut self, id: ClientOrderId, fill: &Fill) {
        if !self.seen_trades.insert(fill.trade_id.clone()) {
            return;
        }
        let Some(stock) = self.stocks.get_mut(fill.instrument.index()) else {
            return;
        };
        let realized = stock.position.apply(fill);
        let strategy = stock.owner;
        if let Some(owner) = strategy {
            match stock.realized.iter_mut().find(|(s, _)| *s == owner) {
                Some((_, total)) => *total += realized,
                None => stock.realized.push((owner, realized)),
            }
        }
        let flat = stock.position.qty == 0;
        self.events.push(OmsEvent::Filled {
            fill: fill.clone(),
            strategy,
        });

        let mut exit_complete = false;
        if let Some(order) = self.orders.get_mut(&id)
            && fill.side == order.side
        {
            order.filled = order.filled.saturating_add(fill.qty);
            exit_complete = order.role == Role::Exit && order.filled >= order.qty;
            if order.role == Role::Entry && order.filled >= order.qty {
                self.report_entry_finished(id);
            }
        }
        if exit_complete && let Some(order) = self.orders.get_mut(&id) {
            order.done = true;
            if let Some(stock) = self.stocks.get_mut(fill.instrument.index()) {
                stock.exit = None;
            }
        }

        // Flat with a fully filled entry: its stop or target has done its
        // job, and the broker has dropped the other leg.
        if flat
            && let Some(entry) = self
                .stocks
                .get(fill.instrument.index())
                .and_then(|s| s.entry)
            && self.orders.get(&entry).is_some_and(|o| o.filled >= o.qty)
        {
            if let Some(order) = self.orders.get_mut(&entry) {
                order.done = true;
            }
            if let Some(stock) = self.stocks.get_mut(fill.instrument.index()) {
                stock.entry = None;
                stock.exit_after_cancel = false;
            }
        }
        self.release_owner(fill.instrument);
    }

    /// Sets positions from the broker after a restart. Nothing owns them,
    /// so strategies leave them alone; their stops stay at the broker and
    /// they are closed at square-off.
    pub fn restore(
        &mut self,
        positions: &[PositionSnapshot],
        now: Timestamp,
    ) -> Result<(), OmsError> {
        for stock in &mut self.stocks {
            stock.position = Position::flat(stock.position.instrument);
            stock.owner = None;
            stock.entry = None;
            stock.exit = None;
            stock.exit_after_cancel = false;
        }
        let client_id =
            ClientOrderId::new("restore").map_err(|e| OmsError::Invalid(e.to_string()))?;
        for s in positions.iter().filter(|s| s.qty != 0) {
            let price = s.avg_price.ok_or_else(|| {
                OmsError::Invalid(format!("no average price for {}", s.instrument))
            })?;
            let qty = u32::try_from(s.qty.unsigned_abs())
                .map_err(|_| OmsError::Invalid(format!("bad quantity {}", s.qty)))?;
            let stock = self
                .stocks
                .get_mut(s.instrument.index())
                .ok_or(OmsError::UnknownInstrument(s.instrument))?;
            stock.position.apply(&Fill {
                client_id,
                instrument: s.instrument,
                side: if s.qty > 0 { Side::Buy } else { Side::Sell },
                qty,
                price,
                ts: now,
                trade_id: "restore".into(),
            });
        }
        Ok(())
    }

    /// Stocks where our position differs from the broker's.
    pub fn reconcile(&self, broker: &[PositionSnapshot]) -> Vec<Mismatch> {
        let theirs = |id: InstrumentId| {
            broker
                .iter()
                .find(|s| s.instrument == id)
                .map_or(0, |s| s.qty)
        };
        let mut out: Vec<Mismatch> = self
            .stocks
            .iter()
            .filter(|s| s.position.qty != theirs(s.position.instrument))
            .map(|s| Mismatch {
                instrument: s.position.instrument,
                ours: s.position.qty,
                broker: theirs(s.position.instrument),
            })
            .collect();
        out.extend(
            broker
                .iter()
                .filter(|s| s.qty != 0 && s.instrument.index() >= self.stocks.len())
                .map(|s| Mismatch {
                    instrument: s.instrument,
                    ours: 0,
                    broker: s.qty,
                }),
        );
        out
    }
}

#[cfg(test)]
mod tests;
