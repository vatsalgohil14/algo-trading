//! Building blocks every strategy needs, so a new strategy only writes its
//! idea.
//!
//! * [`Daily`]: per-stock state that starts fresh each trading day.
//! * [`entry_allowed`]: every rule that must hold before any new position,
//!   whatever the strategy. Skipping one of these is how a strategy trades on
//!   a dead feed or after the entry cut-off; with this, it can't forget.
//! * [`Signals`]: well-formed signals. They act from the next minute, stops
//!   and targets sit on the tick grid and round to the safe side.
//!
//! ```
//! # use strategies::kit::{Daily, Signals, entry_allowed};
//! # #[derive(Default)] struct Today { traded: bool }
//! # fn rules(bar: &trading_core::Bar, ctx: &strategies::Context<'_>,
//! #          days: &mut Daily<Today>, signals: &Signals,
//! #          out: &mut Vec<trading_core::Signal>) -> Option<()> {
//! let today = days.today(bar)?;               // reset each morning
//! if today.traded || entry_allowed(bar, ctx).is_err() {
//!     return None;
//! }
//! let stop = bar.low;
//! let target = bar.close.offset_pct(1.0)?;
//! out.push(signals.enter_long(bar, ctx, stop, Some(target), "my_setup")?);
//! today.traded = true;
//! # Some(())
//! # }
//! ```

use trading_core::{Bar, InstrumentId, Intent, Price, Signal, StrategyId, TickRounding};

use crate::Context;

/// Per-stock state of type `T` that is reset to `T::default()` on the first
/// bar of each IST trading day.
#[derive(Debug, Clone)]
pub struct Daily<T> {
    days: Vec<Option<i64>>,
    states: Vec<T>,
}

impl<T: Default + Clone> Daily<T> {
    /// For `instruments` stocks, `InstrumentId(0)..InstrumentId(n - 1)`.
    pub fn new(instruments: usize) -> Self {
        Self {
            days: vec![None; instruments],
            states: vec![T::default(); instruments],
        }
    }
}

impl<T: Default> Daily<T> {
    /// Today's state for the bar's stock, fresh if this is the day's first
    /// bar. `None` for a stock outside the universe.
    pub fn today(&mut self, bar: &Bar) -> Option<&mut T> {
        let i = bar.instrument.index();
        let day = bar.ts.ist_day();
        let (seen, state) = (self.days.get_mut(i)?, self.states.get_mut(i)?);
        if *seen != Some(day) {
            *seen = Some(day);
            *state = T::default();
        }
        Some(state)
    }

    /// The stock's state as last left, without resetting it.
    pub fn get(&self, instrument: InstrumentId) -> Option<&T> {
        self.states.get(instrument.index())
    }
}

/// Why a new position may not be opened now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Blocked {
    /// Market data is not flowing normally.
    FeedDown,
    /// The stock may not be traded intraday today (surveillance, corporate
    /// action, near a circuit limit, MIS not allowed).
    NotTradable,
    /// Past the last entry time of the session.
    TooLate,
    /// Already holding a position in this stock.
    NotFlat,
}

impl Blocked {
    /// Short name for filter counts in reports.
    pub const fn name(self) -> &'static str {
        match self {
            Blocked::FeedDown => "feed_down",
            Blocked::NotTradable => "not_tradable",
            Blocked::TooLate => "too_late",
            Blocked::NotFlat => "not_flat",
        }
    }
}

/// Checks the rules every entry must pass: feed healthy, stock tradable
/// today, before the session's last entry time, and flat in this stock.
pub fn entry_allowed(bar: &Bar, ctx: &Context<'_>) -> Result<(), Blocked> {
    if !ctx.feed_ok {
        return Err(Blocked::FeedDown);
    }
    if !ctx.tradable {
        return Err(Blocked::NotTradable);
    }
    if !ctx.hours.entries_allowed(bar.ts) {
        return Err(Blocked::TooLate);
    }
    if !ctx.position.is_flat() {
        return Err(Blocked::NotFlat);
    }
    Ok(())
}

/// Makes signals for one strategy.
///
/// Every signal is stamped one minute after the bar it was decided on, the
/// earliest it can be acted on, because the bar's close is only known at
/// its end. Stops round away from the entry and targets to the nearest
/// tick, so rounding never adds risk. Entries that would fail
/// [`Signal::validate`] come back as `None` instead of being sent.
#[derive(Debug, Clone, Copy)]
pub struct Signals {
    strategy: StrategyId,
}

impl Signals {
    pub const fn new(strategy: StrategyId) -> Self {
        Self { strategy }
    }

    /// Buy at market, protected by `stop`.
    pub fn enter_long(
        &self,
        bar: &Bar,
        ctx: &Context<'_>,
        stop: Price,
        target: Option<Price>,
        reason: &'static str,
    ) -> Option<Signal> {
        self.entry(bar, ctx, Intent::EnterLong, stop, target, reason)
    }

    /// Sell short at market, protected by `stop`.
    pub fn enter_short(
        &self,
        bar: &Bar,
        ctx: &Context<'_>,
        stop: Price,
        target: Option<Price>,
        reason: &'static str,
    ) -> Option<Signal> {
        self.entry(bar, ctx, Intent::EnterShort, stop, target, reason)
    }

    /// Close the position and cancel working orders in this stock.
    pub fn exit(&self, bar: &Bar, reason: &'static str) -> Signal {
        Signal {
            strategy: self.strategy,
            instrument: bar.instrument,
            ts: bar.ts.plus_minutes(1),
            intent: Intent::Exit,
            entry: None,
            stop: None,
            target: None,
            reason,
        }
    }

    /// Move the protective stop of a long (`long = true`) or short position.
    /// The risk manager refuses moves that widen risk.
    pub fn move_stop(
        &self,
        bar: &Bar,
        ctx: &Context<'_>,
        long: bool,
        stop: Price,
        reason: &'static str,
    ) -> Option<Signal> {
        let rounding = if long {
            TickRounding::Down
        } else {
            TickRounding::Up
        };
        let stop = stop.round_to_tick(ctx.instrument.tick_size, rounding);
        stop.is_positive().then(|| Signal {
            strategy: self.strategy,
            instrument: bar.instrument,
            ts: bar.ts.plus_minutes(1),
            intent: Intent::AdjustStop,
            entry: None,
            stop: Some(stop),
            target: None,
            reason,
        })
    }

    fn entry(
        &self,
        bar: &Bar,
        ctx: &Context<'_>,
        intent: Intent,
        stop: Price,
        target: Option<Price>,
        reason: &'static str,
    ) -> Option<Signal> {
        let tick = ctx.instrument.tick_size;
        let stop_rounding = if intent == Intent::EnterLong {
            TickRounding::Down
        } else {
            TickRounding::Up
        };
        let signal = Signal {
            strategy: self.strategy,
            instrument: bar.instrument,
            ts: bar.ts.plus_minutes(1),
            intent,
            entry: None,
            stop: Some(stop.round_to_tick(tick, stop_rounding)),
            target: target.map(|t| t.round_to_tick(tick, TickRounding::Nearest)),
            reason,
        };
        signal.validate(bar.close).ok().map(|()| signal)
    }
}

#[cfg(test)]
mod tests {
    use indicators::{BankBuilder, Registry};
    use trading_core::{Exchange, MarketHours, Timestamp, Universe};

    use super::*;
    use crate::PositionView;

    fn bar(instrument: u32, ts: Timestamp, close: i64) -> Bar {
        let p = Price::from_paise(close);
        Bar {
            instrument: InstrumentId(instrument),
            ts,
            open: p,
            high: p,
            low: p,
            close: p,
            volume: 10,
        }
    }

    fn at(day: u32, minute: i64) -> Timestamp {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 1, day).unwrap();
        MarketHours::NSE_EQUITY
            .open_at(date)
            .unwrap()
            .plus_minutes(minute)
    }

    #[test]
    fn daily_state_resets_each_day_and_per_stock() {
        let mut d: Daily<u32> = Daily::new(2);
        *d.today(&bar(0, at(6, 0), 100)).unwrap() += 1;
        *d.today(&bar(0, at(6, 5), 100)).unwrap() += 1;
        *d.today(&bar(1, at(6, 5), 100)).unwrap() += 7;
        assert_eq!(d.get(InstrumentId(0)), Some(&2));
        assert_eq!(*d.today(&bar(0, at(7, 0), 100)).unwrap(), 0);
        assert_eq!(d.get(InstrumentId(1)), Some(&7));
        assert!(d.today(&bar(5, at(7, 0), 100)).is_none());
    }

    #[test]
    fn gate_and_signals() {
        let mut universe = Universe::new();
        universe
            .add(Exchange::Nse, "S0", 1, Price::from_paise(5), 1)
            .unwrap();
        let bank = BankBuilder::new(Registry::builtin()).build(1).unwrap();
        let mut ctx = Context {
            instrument: universe.get(InstrumentId(0)).unwrap(),
            bank: &bank,
            session: indicators::BarContext {
                new_session: false,
                session_minute: Some(30),
            },
            hours: MarketHours::NSE_EQUITY,
            feed_ok: true,
            tradable: true,
            position: PositionView::FLAT,
        };
        let b = bar(0, at(6, 30), 10_000);
        assert_eq!(entry_allowed(&b, &ctx), Ok(()));
        assert_eq!(
            entry_allowed(&bar(0, at(6, 340), 10_000), &ctx),
            Err(Blocked::TooLate)
        );
        ctx.feed_ok = false;
        assert_eq!(entry_allowed(&b, &ctx), Err(Blocked::FeedDown));
        ctx.feed_ok = true;
        ctx.position.qty = 5;
        assert_eq!(entry_allowed(&b, &ctx), Err(Blocked::NotFlat));
        ctx.position.qty = 0;

        let s = Signals::new(StrategyId(3));
        // Stop 98.03 rounds down to 98.00, target 102.03 to 102.05.
        let long = s
            .enter_long(
                &b,
                &ctx,
                Price::from_paise(9_803),
                Some(Price::from_paise(10_203)),
                "t",
            )
            .unwrap();
        assert_eq!(long.ts, at(6, 31));
        assert_eq!(long.stop, Some(Price::from_paise(9_800)));
        assert_eq!(long.target, Some(Price::from_paise(10_205)));
        assert_eq!(long.strategy, StrategyId(3));
        // Short stop rounds up.
        let short = s
            .enter_short(&b, &ctx, Price::from_paise(10_197), None, "t")
            .unwrap();
        assert_eq!(short.stop, Some(Price::from_paise(10_200)));
        // A stop on the wrong side never becomes a signal.
        assert!(
            s.enter_long(&b, &ctx, Price::from_paise(10_100), None, "t")
                .is_none()
        );
        assert_eq!(s.exit(&b, "x").intent, Intent::Exit);
        let moved = s
            .move_stop(&b, &ctx, true, Price::from_paise(9_952), "be")
            .unwrap();
        assert_eq!(moved.stop, Some(Price::from_paise(9_950)));
    }
}
