//! Turns fills into finished trades for reports.

use costs::CostModel;
use risk::Approved;
use trading_core::{Fill, InstrumentId, Money, Price, Side, Signal, StrategyId, Timestamp};

/// One trade from flat to flat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedTrade {
    pub instrument: InstrumentId,
    /// `None` for a position no strategy opened.
    pub strategy: Option<StrategyId>,
    /// The entry signal's reason, e.g. `orb_long`.
    pub setup: &'static str,
    /// The exit signal's reason, or `stop_or_target` if it closed at the
    /// broker.
    pub exit_reason: &'static str,
    pub side: Side,
    pub qty: u32,
    pub entry_price: Price,
    pub exit_price: Price,
    pub opened: Timestamp,
    pub closed: Timestamp,
    pub gross: Money,
    pub charges: Money,
    pub net: Money,
    /// Planned loss at the stop, for measuring results in R.
    pub planned_risk: Money,
}

impl ClosedTrade {
    /// Net result as a multiple of the planned risk.
    pub fn r_multiple(&self) -> Option<f64> {
        (self.planned_risk.paise() > 0).then(|| self.net.as_f64() / self.planned_risk.as_f64())
    }
}

#[derive(Debug, Clone, Copy)]
struct Plan {
    strategy: StrategyId,
    setup: &'static str,
    planned_risk: Money,
}

#[derive(Debug, Clone, Copy)]
struct Open {
    plan: Option<Plan>,
    side: Side,
    opened: Timestamp,
    closed: Timestamp,
    bought_qty: u64,
    bought_value: i128,
    sold_qty: u64,
    sold_value: i128,
    charges: Money,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    plan: Option<Plan>,
    open: Option<Open>,
    exit_reason: Option<&'static str>,
}

#[derive(Debug)]
pub(crate) struct Journal {
    model: CostModel,
    slots: Vec<Slot>,
    closed: Vec<ClosedTrade>,
}

fn average(value: i128, qty: u64) -> Price {
    if qty == 0 {
        return Price::ZERO;
    }
    let qty = i128::from(qty);
    Price::from_paise(i64::try_from((value + qty / 2) / qty).unwrap_or(i64::MAX))
}

impl Journal {
    pub(crate) fn new(model: CostModel, instruments: usize) -> Self {
        Self {
            model,
            slots: vec![Slot::default(); instruments],
            closed: Vec::new(),
        }
    }

    pub(crate) fn take_closed(&mut self) -> Vec<ClosedTrade> {
        std::mem::take(&mut self.closed)
    }

    pub(crate) fn on_entry(&mut self, i: InstrumentId, signal: &Signal, approved: &Approved) {
        if let Some(slot) = self.slots.get_mut(i.index()) {
            slot.plan = Some(Plan {
                strategy: signal.strategy,
                setup: signal.reason,
                planned_risk: approved.max_loss,
            });
            slot.exit_reason = None;
        }
    }

    pub(crate) fn on_exit_signal(&mut self, i: InstrumentId, reason: &'static str) {
        if let Some(slot) = self.slots.get_mut(i.index())
            && slot.open.is_some()
            && slot.exit_reason.is_none()
        {
            slot.exit_reason = Some(reason);
        }
    }

    pub(crate) fn on_fill(&mut self, fill: &Fill, _strategy: Option<StrategyId>) {
        let charges = self.model.charges(fill.side, fill.price, fill.qty).total();
        let Some(slot) = self.slots.get_mut(fill.instrument.index()) else {
            return;
        };
        let plan = slot.plan;
        let open = slot.open.get_or_insert(Open {
            plan,
            side: fill.side,
            opened: fill.ts,
            closed: fill.ts,
            bought_qty: 0,
            bought_value: 0,
            sold_qty: 0,
            sold_value: 0,
            charges: Money::ZERO,
        });
        let value = i128::from(fill.price.paise()) * i128::from(fill.qty);
        match fill.side {
            Side::Buy => {
                open.bought_qty += u64::from(fill.qty);
                open.bought_value += value;
            }
            Side::Sell => {
                open.sold_qty += u64::from(fill.qty);
                open.sold_value += value;
            }
        }
        open.charges += charges;
        open.closed = fill.ts;
    }

    /// `qty` is the net position after the fill just recorded.
    pub(crate) fn after_fill(&mut self, i: InstrumentId, qty: i64) {
        if qty != 0 {
            return;
        }
        let Some(slot) = self.slots.get_mut(i.index()) else {
            return;
        };
        let Some(open) = slot.open.take() else {
            return;
        };
        let reason = slot.exit_reason.take().unwrap_or("stop_or_target");
        slot.plan = None;
        let gross = Money::from_paise(
            i64::try_from(open.sold_value - open.bought_value).unwrap_or(i64::MAX),
        );
        let avg_buy = average(open.bought_value, open.bought_qty);
        let avg_sell = average(open.sold_value, open.sold_qty);
        let (entry_price, exit_price) = match open.side {
            Side::Buy => (avg_buy, avg_sell),
            Side::Sell => (avg_sell, avg_buy),
        };
        self.closed.push(ClosedTrade {
            instrument: i,
            strategy: open.plan.map(|p| p.strategy),
            setup: open.plan.map_or("no_setup", |p| p.setup),
            exit_reason: reason,
            side: open.side,
            qty: u32::try_from(open.bought_qty).unwrap_or(u32::MAX),
            entry_price,
            exit_price,
            opened: open.opened,
            closed: open.closed,
            gross,
            charges: open.charges,
            net: gross - open.charges,
            planned_risk: open.plan.map_or(Money::ZERO, |p| p.planned_risk),
        });
    }
}
