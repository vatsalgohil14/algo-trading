//! Bar-by-bar tests of the ORB rules.
//!
//! Every stock has three history days at ₹500 (daily range ₹10, so the
//! daily ATR is ₹10) and then a test day whose opening range is
//! ₹498-₹502 (width ₹4 = 0.4 ATR). With the defaults a long breakout needs
//! a close at or above ₹502.20 (buffer 5% of the width).

use chrono::NaiveDate;
use indicators::{BankBuilder, IndicatorBank, Registry};
use trading_core::{
    Bar, Exchange, InstrumentId, Intent, MarketHours, Money, Price, Signal, StrategyId, Universe,
};

use super::*;
use crate::{Context, PositionView, Strategy};

fn p(rupees: f64) -> Price {
    Price::from_rupees_f64(rupees).unwrap()
}

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 1, day).unwrap()
}

/// Test day (a Thursday, after history on Mon-Wed).
const TODAY: u32 = 9;

fn config() -> OrbConfig {
    OrbConfig {
        atr_days: 2,
        turnover_days: 2,
        min_turnover_cr: 10.0,
        volume_bars: 5,
        ..OrbConfig::default()
    }
}

struct H {
    bank: IndicatorBank,
    orb: Orb,
    universe: Universe,
    pos: Vec<PositionView>,
    feed_ok: bool,
    tradable: bool,
    /// Rejection counts after the history days (which have no history of
    /// their own, so they are rejected too).
    baseline: Vec<u64>,
}

impl H {
    fn new(config: OrbConfig, stocks: u32) -> Self {
        let mut universe = Universe::new();
        for i in 0..stocks {
            universe
                .add(
                    Exchange::Nse,
                    &format!("S{i}"),
                    100 + i,
                    Price::from_paise(5),
                    1,
                )
                .unwrap();
        }
        let mut b = BankBuilder::new(Registry::builtin());
        let orb = Orb::new(StrategyId(0), config, &mut b, stocks as usize).unwrap();
        let mut h = Self {
            bank: b.build(stocks as usize).unwrap(),
            orb,
            universe,
            pos: vec![PositionView::FLAT; stocks as usize],
            feed_ok: true,
            tradable: true,
            baseline: Vec::new(),
        };
        for i in 0..stocks {
            for day in 6..=8 {
                for m in 0..375 {
                    let (hi, lo) = if m == 0 {
                        (505.0, 495.0)
                    } else {
                        (500.0, 500.0)
                    };
                    let out = h.bar_on(i, date(day), m, [500.0, hi, lo, 500.0], 1000);
                    assert!(out.is_empty(), "history day produced {out:?}");
                }
            }
        }
        h.baseline = h.orb.rejections().map(|(_, n)| n).collect();
        h
    }

    fn bar_on(
        &mut self,
        inst: u32,
        day: NaiveDate,
        minute: i64,
        ohlc: [f64; 4],
        volume: u64,
    ) -> Vec<Signal> {
        let open = MarketHours::NSE_EQUITY.open_at(day).unwrap();
        let bar = Bar {
            instrument: InstrumentId(inst),
            ts: open.plus_minutes(minute),
            open: p(ohlc[0]),
            high: p(ohlc[1]),
            low: p(ohlc[2]),
            close: p(ohlc[3]),
            volume,
        };
        bar.validate().unwrap();
        let session = self.bank.update(&bar).unwrap();
        let ctx = Context {
            instrument: self.universe.get(InstrumentId(inst)).unwrap(),
            bank: &self.bank,
            session,
            hours: MarketHours::NSE_EQUITY,
            feed_ok: self.feed_ok,
            tradable: self.tradable,
            position: self.pos[inst as usize],
        };
        let mut out = Vec::new();
        self.orb.on_bar(&bar, &ctx, &mut out);
        for s in &out {
            s.validate(s.entry.unwrap_or(bar.close)).unwrap();
        }
        out
    }

    /// A bar today.
    fn bar(&mut self, inst: u32, minute: i64, ohlc: [f64; 4], volume: u64) -> Vec<Signal> {
        self.bar_on(inst, date(TODAY), minute, ohlc, volume)
    }

    fn flat(&mut self, inst: u32, minute: i64, price: f64) -> Vec<Signal> {
        self.bar(inst, minute, [price; 4], 1000)
    }

    /// Today's opening range, 498-502, and quiet bars until `until`.
    fn range(&mut self, inst: u32, until: i64) {
        for m in 0..until {
            let ohlc = match m {
                3 => [500.0, 502.0, 499.0, 500.0],
                7 => [500.0, 501.0, 498.0, 500.0],
                _ => [500.0, 501.0, 499.0, 500.0],
            };
            let out = self.bar(inst, m, ohlc, 1000);
            assert!(out.is_empty(), "minute {m} produced {out:?}");
        }
    }

    /// Long breakout bar: close 502.50 on double volume.
    fn breakout(&mut self, inst: u32, minute: i64) -> Vec<Signal> {
        self.bar(inst, minute, [501.0, 502.6, 500.8, 502.5], 2000)
    }

    fn fill(&mut self, inst: u32, qty: i64, price: f64) {
        self.pos[inst as usize] = PositionView {
            qty,
            avg_price: Some(p(price)),
            realized: self.pos[inst as usize].realized,
        };
    }

    fn close_out(&mut self, inst: u32, realized: f64) {
        self.pos[inst as usize] = PositionView {
            qty: 0,
            avg_price: None,
            realized: Money::from_rupees_f64(realized).unwrap(),
        };
    }

    /// Rejections for `name` on the test day(s).
    fn rejected(&self, name: &str) -> u64 {
        let i = self.orb.rejections().position(|(n, _)| n == name).unwrap();
        self.orb.rejections().nth(i).unwrap().1 - self.baseline[i]
    }

    fn total_rejected(&self) -> u64 {
        self.orb.rejections().map(|(_, n)| n).sum::<u64>() - self.baseline.iter().sum::<u64>()
    }

    /// Long trade entered at 503.05 (stop 498, target 513.15) and filled
    /// on the 09:32 bar.
    fn long_trade(config: OrbConfig) -> Self {
        let mut h = Self::new(config, 1);
        h.range(0, 16);
        assert_eq!(h.breakout(0, 16).len(), 1);
        h.fill(0, 10, 503.05);
        assert!(h.bar(0, 17, [503.0, 503.4, 502.9, 503.2], 1500).is_empty());
        h
    }
}

fn only(out: &[Signal]) -> &Signal {
    assert_eq!(out.len(), 1, "expected one signal, got {out:?}");
    &out[0]
}

#[test]
fn long_breakout_entry() {
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    let out = h.breakout(0, 16);
    let s = only(&out);
    assert_eq!(s.intent, Intent::EnterLong);
    assert_eq!(s.reason, "orb_long");
    // 502.50 + 0.1% = 503.0025, rounded up to the 5-paise tick.
    assert_eq!(s.entry, Some(p(503.05)));
    assert_eq!(s.stop, Some(p(498.0)));
    // Risk 5.05, target 2R = 10.10 above the entry.
    assert_eq!(s.target, Some(p(513.15)));
    // Decided at the close of the 09:31 bar.
    assert_eq!(s.ts.ist_minute_of_day(), MinuteOfDay::new(9, 32).unwrap());
    assert_eq!(h.orb.entries_today(), 1);

    // One attempt per stock and day: further breakouts are ignored.
    assert!(h.breakout(0, 17).is_empty());
    assert!(h.breakout(0, 18).is_empty());
}

#[test]
fn short_breakout_is_the_mirror_image() {
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    let out = h.bar(0, 16, [499.0, 499.2, 497.4, 497.5], 2000);
    let s = only(&out);
    assert_eq!(s.intent, Intent::EnterShort);
    // 497.50 - 0.1% = 497.0025, rounded down.
    assert_eq!(s.entry, Some(p(497.0)));
    assert_eq!(s.stop, Some(p(502.0)));
    assert_eq!(s.target, Some(p(487.0)));
}

#[test]
fn needs_a_close_beyond_the_buffer_not_a_spike() {
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    // High pokes through but the close is only 502.10 (< 502.20).
    assert!(h.bar(0, 16, [501.0, 504.0, 500.8, 502.1], 1000).is_empty());
    assert_eq!(h.total_rejected(), 0);
    assert_eq!(only(&h.breakout(0, 17)).intent, Intent::EnterLong);
}

#[test]
fn no_entry_inside_the_opening_range() {
    let mut h = H::new(config(), 1);
    for m in 0..15 {
        // Each bar is far above the previous high, but the range is still forming.
        let px = 500.0 + m as f64;
        assert!(
            h.bar(0, m, [px, px + 0.5, px - 0.5, px + 0.4], 5000)
                .is_empty()
        );
    }
}

#[test]
fn missing_range_bar_means_no_trade_today() {
    let mut h = H::new(config(), 1);
    for m in (0..16).filter(|m| *m != 5) {
        h.flat(0, m, 500.0);
    }
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("range_incomplete"), 1);
}

#[test]
fn day_filters() {
    // Opening gap of 4% over yesterday's 500 close.
    let mut h = H::new(config(), 1);
    for m in 0..16 {
        h.flat(0, m, 520.0);
    }
    assert_eq!(h.rejected("gap"), 1);

    // Range 490-510 is 2x the daily ATR of 10.
    let mut h = H::new(config(), 1);
    h.flat(0, 0, 500.0);
    h.bar(0, 1, [500.0, 510.0, 490.0, 500.0], 1000);
    for m in 2..16 {
        h.flat(0, m, 500.0);
    }
    assert_eq!(h.rejected("range_too_wide"), 1);

    // A flat range is too narrow.
    let mut h = H::new(config(), 1);
    for m in 0..16 {
        h.flat(0, m, 500.0);
    }
    assert_eq!(h.rejected("range_too_narrow"), 1);

    // Turnover of 18.75 crore a day is below 50 crore.
    let mut h = H::new(
        OrbConfig {
            min_turnover_cr: 50.0,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("liquidity"), 1);

    let mut h = H::new(
        OrbConfig {
            min_price: 600.0,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    assert_eq!(h.rejected("price_band"), 1);
}

#[test]
fn not_enough_history_means_no_trade() {
    let mut h = H::new(
        OrbConfig {
            atr_days: 5,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("no_history"), 1);
}

#[test]
fn breakout_filters() {
    // Normal volume is not enough.
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    assert!(h.bar(0, 16, [501.0, 502.6, 500.8, 502.5], 1400).is_empty());
    assert_eq!(h.rejected("volume"), 1);

    // Too far beyond the level (5 > half the 4 range).
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    assert!(h.bar(0, 16, [501.0, 507.0, 500.8, 507.0], 2000).is_empty());
    assert_eq!(h.rejected("chase"), 1);

    // A heavy spike to 530 drags VWAP above the breakout close.
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    h.bar(0, 16, [501.0, 530.0, 501.0, 501.0], 100_000);
    for m in 17..22 {
        h.flat(0, m, 501.0);
    }
    assert!(h.breakout(0, 22).is_empty());
    assert_eq!(h.rejected("vwap"), 1);

    // Feed problems and untradable stocks block entries.
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    h.feed_ok = false;
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("feed_down"), 1);
    h.feed_ok = true;
    h.tradable = false;
    assert!(h.breakout(0, 17).is_empty());
    assert_eq!(h.rejected("not_tradable"), 1);
    h.tradable = true;
    let out = h.bar(0, 18, [501.0, 502.6, 500.8, 502.5], 5000);
    assert_eq!(only(&out).intent, Intent::EnterLong);

    // Direction switched off.
    let mut h = H::new(
        OrbConfig {
            allow_long: false,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("direction_disabled"), 1);

    // Stop 1.0% away is beyond a 0.8% cap.
    let mut h = H::new(
        OrbConfig {
            max_stop_pct: 0.8,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("stop_too_wide"), 1);

    // A 2% target is below a 3% minimum.
    let mut h = H::new(
        OrbConfig {
            min_target_pct: 3.0,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    assert!(h.breakout(0, 16).is_empty());
    assert_eq!(h.rejected("target_too_small"), 1);
}

#[test]
fn no_entries_after_the_entry_window() {
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    // 11:29 is the last bar that may trigger (it closes at 11:30).
    for m in 16..134 {
        h.flat(0, m, 500.0);
    }
    assert_eq!(only(&h.breakout(0, 134)).intent, Intent::EnterLong);

    let mut h = H::new(config(), 1);
    h.range(0, 16);
    for m in 16..135 {
        h.flat(0, m, 500.0);
    }
    assert!(h.breakout(0, 135).is_empty()); // 11:30
}

#[test]
fn failed_breakout_exits_early() {
    let mut h = H::long_trade(config());
    // 501.10 is still inside the tolerance (502 - 0.25 * 4 = 501).
    assert!(h.flat(0, 18, 501.1).is_empty());
    let out = h.flat(0, 19, 500.9);
    let s = only(&out);
    assert_eq!((s.intent, s.reason), (Intent::Exit, "failed_breakout"));
    // Exit is repeated until the position is gone.
    assert_eq!(only(&h.flat(0, 20, 500.9)).reason, "exit_retry");
    h.close_out(0, -21.5);
    assert!(h.flat(0, 21, 500.9).is_empty());
    assert_eq!(h.orb.losses_today(), 1);
    // Done for the day.
    assert!(h.breakout(0, 22).is_empty());
}

#[test]
fn stop_moves_to_breakeven_at_one_r() {
    let mut h = H::long_trade(config());
    // 1R = 5.05: needs a high of 508.10.
    assert!(h.bar(0, 18, [503.2, 508.05, 503.1, 507.0], 1000).is_empty());
    let out = h.bar(0, 19, [507.0, 508.1, 506.5, 507.5], 1000);
    let s = only(&out);
    assert_eq!((s.intent, s.stop), (Intent::AdjustStop, Some(p(503.05))));
    // Only once.
    assert!(h.bar(0, 20, [507.5, 509.0, 507.0, 508.0], 1000).is_empty());
    // Closing below the new stop with the position still open: backstop.
    assert_eq!(only(&h.flat(0, 21, 503.0)).reason, "stop_backstop");
}

#[test]
fn time_stop_exits_trades_going_nowhere() {
    let mut h = H::long_trade(config());
    // Filled on the 09:32 bar; 60 minutes later is the bar closing 10:32.
    for m in 18..76 {
        assert!(h.flat(0, m, 503.5).is_empty(), "minute {m}");
    }
    assert_eq!(only(&h.flat(0, 76, 503.5)).reason, "time_stop");
}

#[test]
fn time_stop_spares_trades_that_moved() {
    let mut h = H::long_trade(config());
    // +0.5R = 2.53 above the entry.
    h.bar(0, 18, [503.5, 505.6, 503.5, 505.0], 1000);
    for m in 19..120 {
        assert!(h.flat(0, m, 505.0).is_empty(), "minute {m}");
    }
}

#[test]
fn backstop_when_the_broker_stop_did_not_work() {
    let mut h = H::long_trade(config());
    assert_eq!(only(&h.flat(0, 18, 497.5)).reason, "stop_backstop");
}

#[test]
fn broker_stop_or_target_ends_the_trade() {
    let mut h = H::long_trade(config());
    h.close_out(0, 101.0); // target hit at the broker
    assert!(h.flat(0, 18, 513.0).is_empty());
    assert_eq!(h.orb.losses_today(), 0);
    assert!(h.breakout(0, 19).is_empty());
}

#[test]
fn unfilled_entry_is_cancelled_and_a_late_fill_closed() {
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    h.breakout(0, 16);
    // Signal at 09:32; timeout 3 minutes: cancelled at the 09:35 close.
    assert!(h.flat(0, 17, 502.0).is_empty());
    assert!(h.flat(0, 18, 502.0).is_empty());
    assert_eq!(only(&h.flat(0, 19, 502.0)).reason, "entry_timeout");
    h.fill(0, 10, 503.05);
    assert_eq!(only(&h.flat(0, 20, 503.0)).reason, "unexpected_position");
}

#[test]
fn wrong_side_position_is_closed() {
    let mut h = H::new(config(), 1);
    h.range(0, 16);
    h.breakout(0, 16);
    h.fill(0, -10, 503.0);
    assert_eq!(only(&h.flat(0, 17, 503.0)).reason, "unexpected_position");
}

#[test]
fn squares_off_before_the_close() {
    let cfg = OrbConfig {
        time_stop_minutes: 0,
        ..config()
    };
    let mut h = H::long_trade(cfg);
    for m in 18..354 {
        assert!(h.flat(0, m, 506.0).is_empty(), "minute {m}");
    }
    // The 15:09 bar closes at 15:10.
    assert_eq!(only(&h.flat(0, 354, 506.0)).reason, "square_off");
}

#[test]
fn position_left_from_yesterday_is_closed() {
    let mut h = H::new(config(), 1);
    h.fill(0, 5, 500.0);
    let out = h.flat(0, 0, 500.0);
    assert_eq!(only(&out).reason, "stale_position");
}

#[test]
fn strategy_wide_limits() {
    let mut h = H::new(
        OrbConfig {
            max_open: 1,
            ..config()
        },
        2,
    );
    h.range(0, 16);
    h.range(1, 16);
    assert_eq!(h.breakout(0, 16).len(), 1);
    assert!(h.breakout(1, 16).is_empty());
    assert_eq!(h.rejected("max_open"), 1);

    let mut h = H::new(
        OrbConfig {
            max_losses_per_day: 1,
            ..config()
        },
        2,
    );
    h.range(0, 16);
    h.range(1, 16);
    h.breakout(0, 16);
    h.fill(0, 10, 503.05);
    h.flat(0, 17, 503.0);
    h.close_out(0, -50.0); // stopped out at the broker
    h.flat(0, 18, 497.0);
    assert_eq!(h.orb.losses_today(), 1);
    for m in 16..19 {
        h.flat(1, m, 500.0);
    }
    assert!(h.breakout(1, 19).is_empty());
    assert_eq!(h.rejected("loss_pause"), 1);

    let mut h = H::new(
        OrbConfig {
            max_entries_per_day: 1,
            ..config()
        },
        2,
    );
    h.range(0, 16);
    h.range(1, 16);
    h.breakout(0, 16);
    h.flat(0, 17, 502.0);
    h.flat(0, 18, 502.0);
    h.flat(0, 19, 502.0); // entry timed out, never filled
    for m in 16..20 {
        h.flat(1, m, 500.0);
    }
    assert!(h.breakout(1, 20).is_empty());
    assert_eq!(h.rejected("max_entries"), 1);
}

#[test]
fn counters_reset_each_day() {
    let mut h = H::new(
        OrbConfig {
            max_entries_per_day: 1,
            // The short test day counts towards the next day's average.
            min_turnover_cr: 1.0,
            ..config()
        },
        1,
    );
    h.range(0, 16);
    h.breakout(0, 16);
    assert_eq!(h.orb.entries_today(), 1);
    // Next day: a fresh range and a fresh allowance.
    let next = date(TODAY + 1);
    for m in 0..16 {
        let ohlc = match m {
            3 => [500.0, 502.0, 499.0, 500.0],
            7 => [500.0, 501.0, 498.0, 500.0],
            _ => [500.0, 501.0, 499.0, 500.0],
        };
        h.bar_on(0, next, m, ohlc, 1000);
    }
    let out = h.bar_on(0, next, 16, [501.0, 502.6, 500.8, 502.5], 2000);
    assert_eq!(only(&out).intent, Intent::EnterLong);
}

#[test]
fn invalid_config_is_refused() {
    let mut b = BankBuilder::new(Registry::builtin());
    let bad = OrbConfig {
        target_r: -1.0,
        ..config()
    };
    assert!(Orb::new(StrategyId(0), bad, &mut b, 1).is_err());
}
