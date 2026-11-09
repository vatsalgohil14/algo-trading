use std::collections::BTreeMap;

use costs::CostModel;
use trading_core::position::Position;
use trading_core::{
    ClientOrderId, Fill, Instrument, InstrumentId, Intent, Money, PositionSnapshot, Price, Side,
    Signal, Timestamp,
};

use crate::{RiskConfig, RiskError, check_leverage};

/// Why an entry or a stop change was refused. Counted for reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum RiskReject {
    Halted,
    UnknownInstrument,
    InvalidSignal,
    NoTarget,
    StockBusy,
    TooExpensive,
    LossCap,
    CostTooHigh,
    GrossExposure,
    NetExposure,
    Margin,
    NoOpenTrade,
    StopNotTighter,
}

impl RiskReject {
    const ALL: [RiskReject; 13] = [
        RiskReject::Halted,
        RiskReject::UnknownInstrument,
        RiskReject::InvalidSignal,
        RiskReject::NoTarget,
        RiskReject::StockBusy,
        RiskReject::TooExpensive,
        RiskReject::LossCap,
        RiskReject::CostTooHigh,
        RiskReject::GrossExposure,
        RiskReject::NetExposure,
        RiskReject::Margin,
        RiskReject::NoOpenTrade,
        RiskReject::StopNotTighter,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            RiskReject::Halted => "halted",
            RiskReject::UnknownInstrument => "unknown_instrument",
            RiskReject::InvalidSignal => "invalid_signal",
            RiskReject::NoTarget => "no_target",
            RiskReject::StockBusy => "stock_busy",
            RiskReject::TooExpensive => "too_expensive",
            RiskReject::LossCap => "loss_cap",
            RiskReject::CostTooHigh => "cost_too_high",
            RiskReject::GrossExposure => "gross_exposure",
            RiskReject::NetExposure => "net_exposure",
            RiskReject::Margin => "margin",
            RiskReject::NoOpenTrade => "no_open_trade",
            RiskReject::StopNotTighter => "stop_not_tighter",
        }
    }
}

/// An entry that may go ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Approved {
    pub side: Side,
    pub qty: u32,
    /// Price the size was worked out at.
    pub entry: Price,
    pub stop: Price,
    pub target: Price,
    pub exposure: Money,
    pub margin: Money,
    /// Loss if filled at `entry` and stopped at `stop`, before costs.
    pub max_loss: Money,
    pub expected_cost: Money,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Enter(Approved),
    /// An exit or a stop change that may go ahead.
    Allow,
    Reject(RiskReject),
}

/// Something the caller must act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskEvent {
    /// The daily loss limit was reached. Close every position and cancel
    /// every working entry. Reported once per day.
    DailyLossHit { pnl: Money },
}

/// Why new trades are stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// Until the next trading day.
    DailyLoss,
    /// Until [`RiskManager::resume`].
    Manual,
}

#[derive(Debug, Clone, Copy)]
struct Reservation {
    long: bool,
    exposure: Money,
    margin: Money,
}

#[derive(Debug, Clone)]
struct Stock {
    position: Position,
    mark: Price,
    reservation: Option<Reservation>,
    stop: Option<Price>,
    leverage: f64,
}

impl Stock {
    fn is_busy(&self) -> bool {
        self.reservation.is_some() || self.position.qty != 0
    }

    fn is_long(&self) -> bool {
        match self.reservation {
            Some(r) if self.position.qty == 0 => r.long,
            _ => self.position.qty > 0,
        }
    }

    /// What this stock ties up: the larger of the position and its
    /// reservation, so a partly filled entry is not counted twice.
    fn exposure(&self) -> Money {
        let held = self.position.exposure(self.mark);
        self.reservation.map_or(held, |r| r.exposure.max(held))
    }

    fn margin(&self) -> Money {
        let held = margin_for(self.position.exposure(self.mark), self.leverage);
        self.reservation.map_or(held, |r| r.margin.max(held))
    }
}

/// Margin for `exposure`, rounded up to the paisa.
fn margin_for(exposure: Money, leverage: f64) -> Money {
    Money::from_paise((exposure.paise() as f64 / leverage).ceil() as i64)
}

fn rupees(amount: f64, name: &str) -> Result<Money, RiskError> {
    Money::from_rupees_f64(amount).ok_or_else(|| RiskError::Config(format!("{name} is too large")))
}

/// Whole lots that fit in `amount` at `each` paise a share.
fn lots_within(amount: Money, each: i64, lot: i64) -> i64 {
    amount.paise().div_euclid(each.saturating_mul(lot)) * lot
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    capital: Money,
    min_exposure: Money,
    max_exposure: Money,
    max_loss: Money,
    max_gross: Money,
    max_net: Money,
    daily_loss: Money,
}

/// See the [crate docs](crate). One instance covers the whole account.
#[derive(Debug)]
pub struct RiskManager {
    limits: Limits,
    reward_multiple: f64,
    scores: BTreeMap<String, f64>,
    model: CostModel,
    stocks: Vec<Stock>,
    day: i64,
    /// Today's realized P&L before charges.
    realized: Money,
    charges: Money,
    /// Sum of the unrealized P&L of all positions, kept up to date.
    unrealized: Money,
    daily_halt: bool,
    manual_halt: bool,
    rejects: [u64; RiskReject::ALL.len()],
}

impl RiskManager {
    /// For a universe of `instruments` stocks. Fails on an invalid config.
    pub fn new(
        config: &RiskConfig,
        model: CostModel,
        instruments: usize,
    ) -> Result<Self, RiskError> {
        config.validate()?;
        let limits = Limits {
            capital: rupees(config.capital, "capital")?,
            min_exposure: rupees(config.min_exposure, "min_exposure")?,
            max_exposure: rupees(config.max_exposure, "max_exposure")?,
            max_loss: rupees(config.max_loss_per_trade, "max_loss_per_trade")?,
            max_gross: rupees(config.max_gross_exposure, "max_gross_exposure")?,
            max_net: rupees(config.max_net_exposure, "max_net_exposure")?,
            daily_loss: rupees(config.daily_loss_limit, "daily_loss_limit")?,
        };
        let stocks = (0..instruments)
            .map(|i| Stock {
                position: Position::flat(InstrumentId(i as u32)),
                mark: Price::ZERO,
                reservation: None,
                stop: None,
                leverage: config.default_leverage,
            })
            .collect();
        Ok(Self {
            limits,
            reward_multiple: config.min_reward_cost_multiple,
            scores: config.setup_scores.clone(),
            model,
            stocks,
            day: i64::MIN,
            realized: Money::ZERO,
            charges: Money::ZERO,
            unrealized: Money::ZERO,
            daily_halt: false,
            manual_halt: false,
            rejects: [0; RiskReject::ALL.len()],
        })
    }

    /// The stock's actual intraday leverage from the broker's margin list.
    pub fn set_leverage(
        &mut self,
        instrument: InstrumentId,
        leverage: f64,
    ) -> Result<(), RiskError> {
        check_leverage(leverage)?;
        let stock = self.stocks.get_mut(instrument.index()).ok_or_else(|| {
            RiskError::Config(format!("instrument {instrument} is outside the universe"))
        })?;
        stock.leverage = leverage;
        Ok(())
    }

    /// Today's P&L so far: realized, unrealized, after charges.
    pub fn day_pnl(&self) -> Money {
        self.realized + self.unrealized - self.charges
    }

    /// Exposure of all positions and working entries.
    pub fn gross_exposure(&self) -> Money {
        self.stocks.iter().map(Stock::exposure).sum()
    }

    pub fn margin_used(&self) -> Money {
        self.stocks.iter().map(Stock::margin).sum()
    }

    /// Longs minus shorts.
    pub fn net_exposure(&self) -> Money {
        self.stocks
            .iter()
            .filter(|s| s.is_busy())
            .map(|s| {
                if s.is_long() {
                    s.exposure()
                } else {
                    -s.exposure()
                }
            })
            .sum()
    }

    /// Why new trades are stopped, if they are.
    pub fn halted(&self) -> Option<Halt> {
        if self.manual_halt {
            Some(Halt::Manual)
        } else if self.daily_halt {
            Some(Halt::DailyLoss)
        } else {
            None
        }
    }

    /// Kill switch: no new trades until [`resume`](Self::resume). Exits
    /// still go through.
    pub fn halt(&mut self) {
        self.manual_halt = true;
    }

    /// Lifts [`halt`](Self::halt). A daily loss halt stays until the next
    /// day.
    pub fn resume(&mut self) {
        self.manual_halt = false;
    }

    /// How often each check refused something, by name, for reports.
    pub fn rejections(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        RiskReject::ALL
            .iter()
            .zip(self.rejects)
            .map(|(r, n)| (r.name(), n))
    }

    fn roll_day(&mut self, ts: Timestamp) {
        let day = ts.ist_day();
        if day > self.day {
            self.day = day;
            self.realized = Money::ZERO;
            self.charges = Money::ZERO;
            self.daily_halt = false;
        }
    }

    /// Decides on a signal. `reference` is the expected price of a market
    /// entry (usually the last close); it is ignored for priced entries.
    pub fn evaluate(
        &mut self,
        signal: &Signal,
        instrument: &Instrument,
        reference: Price,
    ) -> Decision {
        self.roll_day(signal.ts);
        let result = match signal.intent {
            Intent::Exit => Ok(Decision::Allow),
            Intent::AdjustStop => self.adjust_stop(signal),
            Intent::EnterLong | Intent::EnterShort => self
                .check_entry(signal, instrument, reference)
                .map(|approved| {
                    self.reserve(signal.instrument, &approved);
                    Decision::Enter(approved)
                }),
        };
        match result {
            Ok(decision) => decision,
            Err(r) => {
                if let Some(n) = self.rejects.get_mut(r as usize) {
                    *n = n.saturating_add(1);
                }
                Decision::Reject(r)
            }
        }
    }

    fn adjust_stop(&mut self, signal: &Signal) -> Result<Decision, RiskReject> {
        let stock = self
            .stocks
            .get_mut(signal.instrument.index())
            .ok_or(RiskReject::UnknownInstrument)?;
        let new = signal.stop.ok_or(RiskReject::InvalidSignal)?;
        let current = stock.stop.filter(|_| stock.is_busy());
        let current = current.ok_or(RiskReject::NoOpenTrade)?;
        let tighter = if stock.is_long() {
            new > current
        } else {
            new < current
        };
        if !tighter {
            return Err(RiskReject::StopNotTighter);
        }
        stock.stop = Some(new);
        Ok(Decision::Allow)
    }

    fn check_entry(
        &self,
        signal: &Signal,
        instrument: &Instrument,
        reference: Price,
    ) -> Result<Approved, RiskReject> {
        if self.halted().is_some() {
            return Err(RiskReject::Halted);
        }
        if instrument.id != signal.instrument {
            return Err(RiskReject::UnknownInstrument);
        }
        let stock = self
            .stocks
            .get(signal.instrument.index())
            .ok_or(RiskReject::UnknownInstrument)?;
        signal
            .validate(reference)
            .map_err(|_| RiskReject::InvalidSignal)?;
        let entry = signal.entry.unwrap_or(reference);
        let stop = signal.stop.ok_or(RiskReject::InvalidSignal)?;
        let target = signal.target.ok_or(RiskReject::NoTarget)?;
        if stock.is_busy() {
            return Err(RiskReject::StockBusy);
        }

        let qty = self.size(signal.reason, instrument, entry, stop)?;
        let exposure = entry.notional(qty);
        let reward = (target - entry).notional(qty).abs();
        let cost = self
            .model
            .expected_round_trip_cost(entry, qty, instrument.tick_size);
        if (reward.paise() as f64) < self.reward_multiple * cost.paise() as f64 {
            return Err(RiskReject::CostTooHigh);
        }

        if self.gross_exposure() + exposure > self.limits.max_gross {
            return Err(RiskReject::GrossExposure);
        }
        let long = signal.intent == Intent::EnterLong;
        let signed = if long { exposure } else { -exposure };
        if (self.net_exposure() + signed).abs() > self.limits.max_net {
            return Err(RiskReject::NetExposure);
        }
        let margin = margin_for(exposure, stock.leverage);
        let available = self.limits.capital + self.day_pnl().min(Money::ZERO);
        if self.margin_used() + margin > available {
            return Err(RiskReject::Margin);
        }

        Ok(Approved {
            side: if long { Side::Buy } else { Side::Sell },
            qty,
            entry,
            stop,
            target,
            exposure,
            margin,
            max_loss: (entry - stop).notional(qty).abs(),
            expected_cost: cost,
        })
    }

    fn size(
        &self,
        setup: &str,
        instrument: &Instrument,
        entry: Price,
        stop: Price,
    ) -> Result<u32, RiskReject> {
        let l = &self.limits;
        let lot = i64::from(instrument.lot_size.max(1));
        let each = entry.paise();
        let risk = (entry - stop).paise().abs();
        if each <= 0 || risk <= 0 {
            return Err(RiskReject::InvalidSignal);
        }

        // Smallest size that reaches the minimum exposure.
        let mut minimum = lots_within(l.min_exposure, each, lot);
        if minimum.saturating_mul(each) < l.min_exposure.paise() {
            minimum = minimum.saturating_add(lot);
        }
        let most = lots_within(l.max_exposure, each, lot);
        if minimum > most {
            return Err(RiskReject::TooExpensive);
        }

        let score = self.scores.get(setup).copied().unwrap_or(0.0);
        let range = (l.max_exposure - l.min_exposure).paise() as f64;
        let wanted = l.min_exposure + Money::from_paise((range * score).round() as i64);
        let qty = lots_within(wanted, each, lot).clamp(minimum, most);
        let qty = qty.min(lots_within(l.max_loss, risk, lot));
        if qty < minimum {
            return Err(RiskReject::LossCap);
        }
        u32::try_from(qty).map_err(|_| RiskReject::InvalidSignal)
    }

    fn reserve(&mut self, instrument: InstrumentId, a: &Approved) {
        if let Some(stock) = self.stocks.get_mut(instrument.index()) {
            stock.reservation = Some(Reservation {
                long: a.side == Side::Buy,
                exposure: a.exposure,
                margin: a.margin,
            });
            stock.stop = Some(a.stop);
            stock.mark = a.entry;
        }
    }

    /// The entry order is finished (filled, cancelled, rejected or expired):
    /// from now on only the actual position counts.
    pub fn release(&mut self, instrument: InstrumentId) {
        if let Some(stock) = self.stocks.get_mut(instrument.index()) {
            stock.reservation = None;
            if stock.position.qty == 0 {
                stock.stop = None;
            }
        }
    }

    /// Records one execution, entry or exit. Each fill must be given once;
    /// the order manager removes duplicates by trade id.
    pub fn on_fill(&mut self, fill: &Fill) -> Option<RiskEvent> {
        self.roll_day(fill.ts);
        let cost = self.model.charges(fill.side, fill.price, fill.qty).total();
        let stock = self.stocks.get_mut(fill.instrument.index())?;
        let before = stock.position.unrealized(stock.mark);
        let realized = stock.position.apply(fill);
        stock.mark = fill.price;
        let after = stock.position.unrealized(stock.mark);
        if stock.position.qty == 0 && stock.reservation.is_none() {
            stock.stop = None;
        }
        self.unrealized += after - before;
        self.realized += realized;
        self.charges += cost;
        self.check_daily_loss()
    }

    /// Updates a stock's latest price, e.g. on each bar close.
    pub fn on_mark(
        &mut self,
        instrument: InstrumentId,
        price: Price,
        ts: Timestamp,
    ) -> Option<RiskEvent> {
        self.roll_day(ts);
        let stock = self.stocks.get_mut(instrument.index())?;
        let before = stock.position.unrealized(stock.mark);
        stock.mark = price;
        let after = stock.position.unrealized(price);
        self.unrealized += after - before;
        self.check_daily_loss()
    }

    fn check_daily_loss(&mut self) -> Option<RiskEvent> {
        let pnl = self.day_pnl();
        if self.daily_halt || pnl > -self.limits.daily_loss {
            return None;
        }
        self.daily_halt = true;
        Some(RiskEvent::DailyLossHit { pnl })
    }

    /// Rebuilds the state after a restart: positions from the broker, which
    /// is the source of truth, and today's realized P&L after charges, so
    /// that a restart cannot reset the daily loss limit.
    pub fn restore(
        &mut self,
        positions: &[PositionSnapshot],
        realized_after_charges: Money,
        now: Timestamp,
    ) -> Result<Option<RiskEvent>, RiskError> {
        for stock in &mut self.stocks {
            stock.position = Position::flat(stock.position.instrument);
            stock.reservation = None;
            stock.stop = None;
        }
        let client_id =
            ClientOrderId::new("restore").map_err(|e| RiskError::Restore(e.to_string()))?;
        for snapshot in positions.iter().filter(|s| s.qty != 0) {
            let price = snapshot.avg_price.ok_or_else(|| {
                RiskError::Restore(format!("no average price for {}", snapshot.instrument))
            })?;
            let qty = u32::try_from(snapshot.qty.unsigned_abs())
                .map_err(|_| RiskError::Restore(format!("bad quantity {}", snapshot.qty)))?;
            let stock = self
                .stocks
                .get_mut(snapshot.instrument.index())
                .ok_or_else(|| {
                    RiskError::Restore(format!("unknown instrument {}", snapshot.instrument))
                })?;
            stock.position.apply(&Fill {
                client_id,
                instrument: snapshot.instrument,
                side: if snapshot.qty > 0 {
                    Side::Buy
                } else {
                    Side::Sell
                },
                qty,
                price,
                ts: now,
                trade_id: "restore".into(),
            });
            stock.mark = price;
        }
        self.day = now.ist_day();
        self.realized = realized_after_charges;
        self.charges = Money::ZERO;
        self.unrealized = Money::ZERO;
        self.daily_halt = false;
        Ok(self.check_daily_loss())
    }
}
