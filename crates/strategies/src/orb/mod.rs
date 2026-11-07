//! Opening Range Breakout (ORB), v1. The full rules, with the reasoning
//! behind each, are in `docs/strategies/orb.md`.
//!
//! Per stock and day:
//!
//! 1. **Range.** High and low of the first `range_minutes` bars. Every bar
//!    must be present, otherwise no trade today.
//! 2. **Day filters**, checked once when the range completes: opening gap,
//!    price band, liquidity (average daily turnover), and range width
//!    compared with the daily ATR.
//! 3. **Entry**: a bar *closes* beyond the range by a buffer, before
//!    `entry_end`, on the right side of VWAP, with above-normal volume and
//!    not too far beyond the level. A limit order slightly beyond the
//!    close, stop at the other side of the range, target at `target_r` R.
//! 4. **Management**: exit when the breakout fails (a close back inside the
//!    range), move the stop to breakeven at `breakeven_r` R, exit trades
//!    going nowhere after `time_stop_minutes`, square off at the session's
//!    square-off time.
//! 5. **Limits**: one trade per stock per day, `max_open` at once,
//!    `max_entries_per_day`, and no new trades after `max_losses_per_day`
//!    losers.
//!
//! The stop and target sit at the broker with the entry (a super order).
//! The strategy's own exits are on top of that, and it also exits if a bar
//! closes past the stop while the position is still open (the broker's
//! stop did not work).
//!
//! State comes from the position reported on each bar, not from what the
//! strategy asked for, so a rejected or unfilled entry, a stop hit at the
//! broker, or a restart are all handled the same way.

mod config;

pub use config::OrbConfig;

use indicators::{BankBuilder, Handle, IndicatorBank};
use trading_core::{
    Bar, InstrumentId, Intent, MinuteOfDay, Money, Price, Signal, StrategyId, TickRounding,
    Timestamp,
};

use crate::{Context, PositionView, Strategy, StrategyError};

/// Why a setup or a breakout was not traded. Counted for reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum Reject {
    // Whole day, counted once per stock and day.
    RangeIncomplete,
    NoHistory,
    Gap,
    PriceBand,
    Liquidity,
    RangeTooNarrow,
    RangeTooWide,
    // Per breakout bar.
    Direction,
    FeedDown,
    NotTradable,
    Chase,
    Vwap,
    Volume,
    StopTooWide,
    TargetTooSmall,
    MaxOpen,
    MaxEntries,
    LossPause,
    InvalidSignal,
}

impl Reject {
    const ALL: [Reject; 19] = [
        Reject::RangeIncomplete,
        Reject::NoHistory,
        Reject::Gap,
        Reject::PriceBand,
        Reject::Liquidity,
        Reject::RangeTooNarrow,
        Reject::RangeTooWide,
        Reject::Direction,
        Reject::FeedDown,
        Reject::NotTradable,
        Reject::Chase,
        Reject::Vwap,
        Reject::Volume,
        Reject::StopTooWide,
        Reject::TargetTooSmall,
        Reject::MaxOpen,
        Reject::MaxEntries,
        Reject::LossPause,
        Reject::InvalidSignal,
    ];

    const fn name(self) -> &'static str {
        match self {
            Reject::RangeIncomplete => "range_incomplete",
            Reject::NoHistory => "no_history",
            Reject::Gap => "gap",
            Reject::PriceBand => "price_band",
            Reject::Liquidity => "liquidity",
            Reject::RangeTooNarrow => "range_too_narrow",
            Reject::RangeTooWide => "range_too_wide",
            Reject::Direction => "direction_disabled",
            Reject::FeedDown => "feed_down",
            Reject::NotTradable => "not_tradable",
            Reject::Chase => "chase",
            Reject::Vwap => "vwap",
            Reject::Volume => "volume",
            Reject::StopTooWide => "stop_too_wide",
            Reject::TargetTooSmall => "target_too_small",
            Reject::MaxOpen => "max_open",
            Reject::MaxEntries => "max_entries",
            Reject::LossPause => "loss_pause",
            Reject::InvalidSignal => "invalid_signal",
        }
    }
}

// Each stock tracks counted rejections in a `u32` bitmask.
const _: () = assert!(Reject::ALL.len() <= 32);

#[derive(Debug, Clone, Copy)]
struct Handles {
    orb_high: Handle,
    orb_low: Handle,
    atr: Handle,
    vwap: Handle,
    volume_prev: Handle,
    prev_close: Handle,
    turnover: Handle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    high: Price,
    low: Price,
}

impl Range {
    fn width(self) -> Price {
        self.high - self.low
    }
}

/// What an entry signal asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    long: bool,
    range: Range,
    entry: Price,
    stop: Price,
    /// Realized P&L in the stock before this trade, to measure its result.
    realized_before: Money,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Trade {
    plan: Plan,
    /// Actual average fill price.
    entry: Price,
    since: Timestamp,
    /// Best price reached since the entry (highest for longs).
    best: Price,
    /// Current protective stop.
    stop: Price,
    at_breakeven: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// Waiting for the opening range to complete.
    #[default]
    Watching,
    /// Day filters passed; looking for a breakout.
    Ready(Range),
    /// Entry signal sent at `at`; waiting for the fill.
    Pending {
        plan: Plan,
        at: Timestamp,
    },
    Open(Trade),
    /// Exit signal sent; waiting to be flat.
    Exiting {
        realized_before: Money,
    },
    /// Nothing more today.
    Done,
}

impl Phase {
    /// Holds a position or an entry order.
    const fn is_active(self) -> bool {
        matches!(
            self,
            Phase::Pending { .. } | Phase::Open(_) | Phase::Exiting { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Stock {
    /// IST day this state belongs to.
    day: i64,
    day_open: Option<Price>,
    phase: Phase,
    /// Bit `r` set: rejection `r` already counted for this stock today.
    counted: u32,
}

/// Opening Range Breakout strategy. See the [module docs](self).
#[derive(Debug)]
pub struct Orb {
    id: StrategyId,
    config: OrbConfig,
    entry_end: MinuteOfDay,
    h: Handles,
    stocks: Vec<Stock>,
    /// IST day of the latest bar seen, for the daily counters.
    day: i64,
    entries_today: u16,
    losses_today: u16,
    rejects: [u64; Reject::ALL.len()],
}

/// `p * f`, rounded to the nearest paisa.
fn scale(p: Price, f: f64) -> Price {
    Price::from_paise((p.paise() as f64 * f).round() as i64)
}

fn price(value: Option<f64>) -> Option<Price> {
    Price::from_rupees_f64(value?)
}

impl Orb {
    /// Builds the strategy for a universe of `instruments` stocks, asking
    /// `bank` for the indicators it reads. Fails on an invalid config.
    pub fn new(
        id: StrategyId,
        config: OrbConfig,
        bank: &mut BankBuilder,
        instruments: usize,
    ) -> Result<Self, StrategyError> {
        config.validate()?;
        let entry_end = config::parse_hhmm(&config.entry_end).ok_or(StrategyError::Config {
            strategy: "orb",
            why: "bad entry_end".into(),
        })?;
        let orb = format!("orb({})", config.range_minutes);
        let h = Handles {
            orb_high: bank.require(&format!("{orb}.high"))?,
            orb_low: bank.require(&format!("{orb}.low"))?,
            atr: bank.require(&format!("atr_daily({})", config.atr_days))?,
            vwap: bank.require("vwap")?,
            volume_prev: bank.require(&format!("vol_sma({}).prev", config.volume_bars))?,
            prev_close: bank.require("prev_day.close")?,
            turnover: bank.require(&format!("turnover({})", config.turnover_days))?,
        };
        Ok(Self {
            id,
            config,
            entry_end,
            h,
            stocks: vec![Stock::default(); instruments],
            day: i64::MIN,
            entries_today: 0,
            losses_today: 0,
            rejects: [0; Reject::ALL.len()],
        })
    }

    pub fn config(&self) -> &OrbConfig {
        &self.config
    }

    /// How often each filter stopped a trade, by name, for reports: the
    /// number of stock-days on which it blocked a setup or at least one
    /// breakout.
    pub fn rejections(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        Reject::ALL
            .iter()
            .zip(self.rejects)
            .map(|(r, n)| (r.name(), n))
    }

    /// Entries opened today, across all stocks.
    pub fn entries_today(&self) -> u16 {
        self.entries_today
    }

    /// Losing trades closed today, across all stocks.
    pub fn losses_today(&self) -> u16 {
        self.losses_today
    }

    fn reject(&mut self, stock: &mut Stock, r: Reject) {
        let bit = 1u32 << (r as usize);
        if stock.counted & bit != 0 {
            return;
        }
        stock.counted |= bit;
        if let Some(n) = self.rejects.get_mut(r as usize) {
            *n = n.saturating_add(1);
        }
    }

    fn signal(&self, bar: &Bar, intent: Intent, reason: &'static str) -> Signal {
        Signal {
            strategy: self.id,
            instrument: bar.instrument,
            // Decided when the bar closed.
            ts: bar.ts.plus_minutes(1),
            intent,
            entry: None,
            stop: None,
            target: None,
            reason,
        }
    }

    fn exit(&self, bar: &Bar, reason: &'static str, out: &mut Vec<Signal>) {
        out.push(self.signal(bar, Intent::Exit, reason));
    }

    /// The trade in this stock is over: count it if it lost money.
    fn closed(&mut self, realized_before: Money, pos: PositionView) {
        if pos.realized < realized_before {
            self.losses_today = self.losses_today.saturating_add(1);
        }
    }

    /// Day filters, once the range is complete.
    fn check_day(
        &self,
        ctx: &Context<'_>,
        inst: InstrumentId,
        day_open: Option<Price>,
    ) -> Result<Range, Reject> {
        let bank: &IndicatorBank = ctx.bank;
        let c = &self.config;
        let (Some(high), Some(low)) = (
            price(bank.get(inst, self.h.orb_high)),
            price(bank.get(inst, self.h.orb_low)),
        ) else {
            return Err(Reject::RangeIncomplete);
        };
        let range = Range { high, low };
        let open = day_open.ok_or(Reject::RangeIncomplete)?;
        let prev_close = bank.get(inst, self.h.prev_close).ok_or(Reject::NoHistory)?;
        let turnover = bank.get(inst, self.h.turnover).ok_or(Reject::NoHistory)?;
        let atr = bank
            .get(inst, self.h.atr)
            .filter(|a| *a > 0.0)
            .ok_or(Reject::NoHistory)?;

        if prev_close <= 0.0
            || ((open.as_f64() - prev_close) / prev_close).abs() * 100.0 > c.max_gap_pct
        {
            return Err(Reject::Gap);
        }
        if low.as_f64() < c.min_price || high.as_f64() > c.max_price {
            return Err(Reject::PriceBand);
        }
        if turnover < c.min_turnover_cr * 1e7 {
            return Err(Reject::Liquidity);
        }
        let ratio = range.width().as_f64() / atr;
        if !range.width().is_positive() || ratio < c.min_range_atr {
            return Err(Reject::RangeTooNarrow);
        }
        if ratio > c.max_range_atr {
            return Err(Reject::RangeTooWide);
        }
        Ok(range)
    }

    /// Looks for a breakout on this bar. `Ok(None)`: no breakout.
    fn check_entry(
        &self,
        bar: &Bar,
        ctx: &Context<'_>,
        range: Range,
    ) -> Result<Option<(Plan, Signal)>, Reject> {
        let c = &self.config;
        let tick = ctx.instrument.tick_size;
        let width = range.width();
        let buffer = scale(width, c.breakout_buffer).max(tick);
        let close = bar.close;
        let long = close >= range.high + buffer;
        if !long && close > range.low - buffer {
            return Ok(None);
        }

        if (long && !c.allow_long) || (!long && !c.allow_short) {
            return Err(Reject::Direction);
        }
        if !ctx.feed_ok {
            return Err(Reject::FeedDown);
        }
        if !ctx.tradable {
            return Err(Reject::NotTradable);
        }
        let level = if long { range.high } else { range.low };
        if (close - level).paise().abs() > scale(width, c.max_chase).paise() {
            return Err(Reject::Chase);
        }
        if c.require_vwap {
            let vwap = ctx
                .bank
                .get(bar.instrument, self.h.vwap)
                .ok_or(Reject::Vwap)?;
            let ok = if long {
                close.as_f64() > vwap
            } else {
                close.as_f64() < vwap
            };
            if !ok {
                return Err(Reject::Vwap);
            }
        }
        let normal = ctx
            .bank
            .get(bar.instrument, self.h.volume_prev)
            .filter(|v| *v > 0.0)
            .ok_or(Reject::Volume)?;
        if (bar.volume as f64) < c.volume_mult * normal {
            return Err(Reject::Volume);
        }

        // Round away from the close, so the limit is never tighter than asked.
        let raw = close.paise() as f64
            * (1.0 + c.limit_offset_pct / 100.0 * if long { 1.0 } else { -1.0 });
        let (paise, rounding) = if long {
            ((raw - 1e-6).ceil(), TickRounding::Up)
        } else {
            ((raw + 1e-6).floor(), TickRounding::Down)
        };
        let entry = Price::from_paise(paise as i64).round_to_tick(tick, rounding);
        if !entry.is_positive() {
            return Err(Reject::InvalidSignal);
        }
        let stop = if long { range.low } else { range.high };
        let risk = (entry - stop).paise().abs();
        if risk <= 0 {
            return Err(Reject::InvalidSignal);
        }
        if risk as f64 / entry.paise() as f64 * 100.0 > c.max_stop_pct {
            return Err(Reject::StopTooWide);
        }
        let reward = scale(Price::from_paise(risk), c.target_r);
        let target = if long { entry + reward } else { entry - reward }
            .round_to_tick(tick, TickRounding::Nearest);
        if (target - entry).paise().abs() as f64 / entry.paise() as f64 * 100.0 < c.min_target_pct {
            return Err(Reject::TargetTooSmall);
        }

        let active = self.stocks.iter().filter(|s| s.phase.is_active()).count();
        if active >= usize::from(c.max_open) {
            return Err(Reject::MaxOpen);
        }
        if self.entries_today >= c.max_entries_per_day {
            return Err(Reject::MaxEntries);
        }
        if self.losses_today >= c.max_losses_per_day {
            return Err(Reject::LossPause);
        }

        let intent = if long {
            Intent::EnterLong
        } else {
            Intent::EnterShort
        };
        let signal = Signal {
            entry: Some(entry),
            stop: Some(stop),
            target: Some(target),
            ..self.signal(bar, intent, if long { "orb_long" } else { "orb_short" })
        };
        signal.validate(entry).map_err(|_| Reject::InvalidSignal)?;
        let plan = Plan {
            long,
            range,
            entry,
            stop,
            realized_before: ctx.position.realized,
        };
        Ok(Some((plan, signal)))
    }

    /// Manages an open trade on a bar. Returns the next phase.
    fn manage(
        &self,
        mut t: Trade,
        bar: &Bar,
        ctx: &Context<'_>,
        fill_bar: bool,
        out: &mut Vec<Signal>,
    ) -> Phase {
        let c = &self.config;
        let long = t.plan.long;
        let close = bar.close;
        // On the bar of the fill, part of the bar's range may be from
        // before the fill, so only its close counts.
        let extreme = match (fill_bar, long) {
            (true, _) => close,
            (false, true) => bar.high,
            (false, false) => bar.low,
        };
        t.best = if long {
            t.best.max(extreme)
        } else {
            t.best.min(extreme)
        };
        // Signed distance in our favour, in paise.
        let favour = |p: Price| {
            if long {
                (p - t.entry).paise()
            } else {
                (t.entry - p).paise()
            }
        };
        let risk = (t.entry - t.plan.stop)
            .paise()
            .abs()
            .max(ctx.instrument.tick_size.paise());
        let exiting = Phase::Exiting {
            realized_before: t.plan.realized_before,
        };

        if favour(close) <= favour(t.stop) {
            self.exit(bar, "stop_backstop", out);
            return exiting;
        }
        let range = t.plan.range;
        let depth = scale(range.width(), c.failed_breakout_depth);
        let failed = if long {
            close < range.high - depth
        } else {
            close > range.low + depth
        };
        if failed {
            self.exit(bar, "failed_breakout", out);
            return exiting;
        }
        let held = bar.ts.plus_minutes(1).micros_since(t.since) / 60_000_000;
        if c.time_stop_minutes > 0
            && held >= i64::from(c.time_stop_minutes)
            && (favour(t.best) as f64) < c.time_stop_min_r * risk as f64
        {
            self.exit(bar, "time_stop", out);
            return exiting;
        }
        if c.breakeven_r > 0.0
            && !t.at_breakeven
            && favour(t.best) as f64 >= c.breakeven_r * risk as f64
        {
            let tick = ctx.instrument.tick_size;
            let be = t.entry.round_to_tick(
                tick,
                if long {
                    TickRounding::Up
                } else {
                    TickRounding::Down
                },
            );
            // Only if it tightens the stop and price is still beyond it.
            if favour(be) > favour(t.stop) && favour(close) > favour(be) {
                out.push(Signal {
                    stop: Some(be),
                    ..self.signal(bar, Intent::AdjustStop, "breakeven")
                });
                t.stop = be;
                t.at_breakeven = true;
            }
        }
        Phase::Open(t)
    }

    fn step(&mut self, stock: &mut Stock, bar: &Bar, ctx: &Context<'_>, out: &mut Vec<Signal>) {
        let pos = ctx.position;
        let close_ts = bar.ts.plus_minutes(1);

        // A position the strategy does not expect (e.g. a fill after the
        // entry was given up, or one left over) is closed straight away.
        let unexpected = match stock.phase {
            Phase::Watching | Phase::Ready(_) | Phase::Done => !pos.is_flat(),
            Phase::Pending { plan, .. } | Phase::Open(Trade { plan, .. }) => {
                !pos.is_flat() && (pos.qty > 0) != plan.long
            }
            Phase::Exiting { .. } => false,
        };
        if unexpected {
            self.exit(bar, "unexpected_position", out);
            stock.phase = Phase::Exiting {
                realized_before: pos.realized,
            };
            return;
        }

        if ctx.hours.must_square_off(close_ts) {
            stock.phase = match stock.phase {
                Phase::Open(t) => {
                    self.exit(bar, "square_off", out);
                    Phase::Exiting {
                        realized_before: t.plan.realized_before,
                    }
                }
                Phase::Pending { plan, .. } if !pos.is_flat() => {
                    self.exit(bar, "square_off", out);
                    Phase::Exiting {
                        realized_before: plan.realized_before,
                    }
                }
                Phase::Pending { .. } => {
                    self.exit(bar, "square_off", out); // cancel the entry
                    Phase::Done
                }
                Phase::Exiting { realized_before } if !pos.is_flat() => {
                    self.exit(bar, "square_off", out);
                    Phase::Exiting { realized_before }
                }
                Phase::Exiting { realized_before } => {
                    self.closed(realized_before, pos);
                    Phase::Done
                }
                Phase::Watching | Phase::Ready(_) | Phase::Done => Phase::Done,
            };
            return;
        }

        stock.phase = match stock.phase {
            Phase::Watching => {
                let last_range_bar = self.config.range_minutes.saturating_sub(1);
                match ctx.session.session_minute {
                    Some(m) if m >= last_range_bar => {
                        match self.check_day(ctx, bar.instrument, stock.day_open) {
                            Ok(range) => Phase::Ready(range),
                            Err(r) => {
                                self.reject(stock, r);
                                Phase::Done
                            }
                        }
                    }
                    _ => Phase::Watching,
                }
            }
            Phase::Ready(range) => {
                let late = bar.ts.ist_minute_of_day() >= self.entry_end
                    || !ctx.hours.entries_allowed(bar.ts);
                if late {
                    Phase::Done
                } else {
                    match self.check_entry(bar, ctx, range) {
                        Ok(None) => Phase::Ready(range),
                        Ok(Some((plan, signal))) => {
                            out.push(signal);
                            self.entries_today = self.entries_today.saturating_add(1);
                            Phase::Pending { plan, at: close_ts }
                        }
                        Err(r) => {
                            self.reject(stock, r);
                            Phase::Ready(range)
                        }
                    }
                }
            }
            Phase::Pending { plan, at } => {
                if !pos.is_flat() {
                    let entry = pos.avg_price.unwrap_or(plan.entry);
                    let trade = Trade {
                        plan,
                        entry,
                        since: bar.ts,
                        best: entry,
                        stop: plan.stop,
                        at_breakeven: false,
                    };
                    self.manage(trade, bar, ctx, true, out)
                } else if close_ts.micros_since(at)
                    >= i64::from(self.config.entry_timeout_minutes) * 60_000_000
                {
                    self.exit(bar, "entry_timeout", out); // cancel the entry
                    Phase::Done
                } else {
                    Phase::Pending { plan, at }
                }
            }
            Phase::Open(t) => {
                if pos.is_flat() {
                    // Stopped out or target hit at the broker.
                    self.closed(t.plan.realized_before, pos);
                    Phase::Done
                } else {
                    self.manage(t, bar, ctx, false, out)
                }
            }
            Phase::Exiting { realized_before } => {
                if pos.is_flat() {
                    self.closed(realized_before, pos);
                    Phase::Done
                } else {
                    // Still not out; ask again (the order manager ignores
                    // a repeat while an exit is in flight).
                    self.exit(bar, "exit_retry", out);
                    Phase::Exiting { realized_before }
                }
            }
            Phase::Done => Phase::Done,
        };
    }
}

impl Strategy for Orb {
    fn id(&self) -> StrategyId {
        self.id
    }

    fn name(&self) -> &'static str {
        "orb"
    }

    fn on_bar(&mut self, bar: &Bar, ctx: &Context<'_>, out: &mut Vec<Signal>) {
        let i = bar.instrument.index();
        let Some(mut stock) = self.stocks.get(i).copied() else {
            return;
        };
        let today = bar.ts.ist_day();
        if today > self.day {
            self.day = today;
            self.entries_today = 0;
            self.losses_today = 0;
        }
        if ctx.session.session_minute == Some(0) && stock.day == today {
            stock.day_open = Some(bar.open);
        }
        if stock.day != today {
            stock = Stock {
                day: today,
                day_open: (ctx.session.session_minute == Some(0)).then_some(bar.open),
                phase: Phase::Watching,
                counted: 0,
            };
            if !ctx.position.is_flat() {
                // Intraday only: anything held from before today goes.
                self.exit(bar, "stale_position", out);
                stock.phase = Phase::Exiting {
                    realized_before: ctx.position.realized,
                };
            } else {
                self.step(&mut stock, bar, ctx, out);
            }
        } else {
            self.step(&mut stock, bar, ctx, out);
        }
        if let Some(slot) = self.stocks.get_mut(i) {
            *slot = stock;
        }
    }
}

#[cfg(test)]
mod tests;
