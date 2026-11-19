//! Builds 1-minute candles from live ticks.
//!
//! * A tick counts as a trade only when the day's cumulative volume went up;
//!   quote updates without a trade (depth changes, snapshots) move nothing.
//! * A candle's minute comes from the exchange trade time, so it matches
//!   historical candles, which are stamped the same way.
//! * A candle closes when the first trade of a later minute arrives, or when
//!   the clock passes the end of its minute plus a grace period, so a quiet
//!   stock still gets its candle on time.
//! * Minutes without a trade produce no candle, as in Dhan's history.
//! * Only trades inside the continuous session count.

use crate::{Bar, InstrumentId, MarketHours, Tick, Timestamp};

const MINUTE_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, Default)]
struct Stock {
    /// Cumulative day volume seen on the previous tick.
    last_volume: Option<u64>,
    forming: Option<Bar>,
    /// Start of the last candle handed out, to keep candles in order.
    last_closed: Option<Timestamp>,
}

/// What happened to one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TickOutcome {
    /// A candle that this tick closed (the previous minute's).
    pub closed: Option<Bar>,
    /// The tick was a trade that updated the forming candle.
    pub traded: bool,
}

/// Turns ticks into 1-minute candles, one stock per [`InstrumentId`].
#[derive(Debug, Clone)]
pub struct CandleBuilder {
    hours: MarketHours,
    grace_secs: i64,
    stocks: Vec<Stock>,
    late: u64,
}

impl CandleBuilder {
    /// For `instruments` stocks, `InstrumentId(0)..InstrumentId(n - 1)`.
    pub fn new(instruments: usize, hours: MarketHours) -> Self {
        Self {
            hours,
            grace_secs: 2,
            stocks: vec![Stock::default(); instruments],
            late: 0,
        }
    }

    /// How long after a minute ends a quiet stock's candle is closed
    /// (default 2 s, to allow for feed delay).
    pub fn with_grace_secs(mut self, secs: i64) -> Self {
        self.grace_secs = secs.max(0);
        self
    }

    /// Marks candles up to and including `ts` as already known, e.g. after
    /// loading today's earlier candles from history, so ticks for those
    /// minutes do not produce a second candle.
    pub fn mark_closed(&mut self, instrument: InstrumentId, ts: Timestamp) {
        if let Some(s) = self.stocks.get_mut(instrument.index()) {
            s.last_closed = Some(s.last_closed.map_or(ts, |t| t.max(ts)));
        }
    }

    pub fn on_tick(&mut self, tick: &Tick) -> TickOutcome {
        let mut out = TickOutcome::default();
        let Some(s) = self.stocks.get_mut(tick.instrument.index()) else {
            return out;
        };
        let minute = tick.ts.floor_minute();
        let in_session = self.hours.is_open(minute);

        let delta = match s.last_volume {
            // First tick seen: outside the session it is only a baseline
            // (it carries pre-open volume); inside, the trade it reports
            // starts the candle, with its earlier volume unknown.
            None if !in_session => {
                s.last_volume = Some(tick.volume);
                return out;
            }
            None => 0,
            Some(last) if tick.volume <= last => {
                // No trade since the last tick (or a reset).
                s.last_volume = Some(tick.volume);
                return out;
            }
            Some(last) => tick.volume - last,
        };
        s.last_volume = Some(tick.volume);
        if !in_session || !tick.ltp.is_positive() {
            return out;
        }
        if s.last_closed.is_some_and(|t| minute <= t) {
            // A trade for a minute already handed out: too late to use.
            self.late += 1;
            return out;
        }

        match &mut s.forming {
            Some(bar) if minute < bar.ts => {
                // Out-of-order trade for an earlier minute still open: keep
                // its volume, not its price.
                bar.volume += delta;
            }
            Some(bar) if minute == bar.ts => {
                bar.high = bar.high.max(tick.ltp);
                bar.low = bar.low.min(tick.ltp);
                bar.close = tick.ltp;
                bar.volume += delta;
            }
            forming => {
                if let Some(done) = forming.take() {
                    s.last_closed = Some(done.ts);
                    out.closed = Some(done);
                }
                *forming = Some(Bar {
                    instrument: tick.instrument,
                    ts: minute,
                    open: tick.ltp,
                    high: tick.ltp,
                    low: tick.ltp,
                    close: tick.ltp,
                    volume: delta,
                });
            }
        }
        out.traded = true;
        out
    }

    /// Closes every candle whose minute ended more than the grace period
    /// before `now`. Returned in stock order.
    pub fn close_due(&mut self, now: Timestamp) -> Vec<Bar> {
        let cutoff = MINUTE_SECS + self.grace_secs;
        let mut done = Vec::new();
        for s in &mut self.stocks {
            if let Some(bar) = s.forming
                && now.micros_since(bar.ts) >= cutoff * 1_000_000
            {
                s.forming = None;
                s.last_closed = Some(bar.ts);
                done.push(bar);
            }
        }
        done
    }

    /// The candle still forming for this stock, if it traded this minute.
    pub fn forming(&self, instrument: InstrumentId) -> Option<&Bar> {
        self.stocks.get(instrument.index())?.forming.as_ref()
    }

    /// Trades that arrived after their minute's candle was handed out.
    pub fn late_trades(&self) -> u64 {
        self.late
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Price;
    use chrono::{NaiveDate, NaiveTime};

    fn at(h: u32, m: u32, s: u32) -> Timestamp {
        Timestamp::from_ist(
            NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
            NaiveTime::from_hms_opt(h, m, s).unwrap(),
        )
        .unwrap()
    }

    fn tick(i: u32, ts: Timestamp, rupees: i64, volume: u64) -> Tick {
        Tick {
            instrument: InstrumentId(i),
            ts,
            ltp: Price::from_paise(rupees * 100),
            ltq: 1,
            volume,
            bid: None,
            ask: None,
        }
    }

    fn builder() -> CandleBuilder {
        CandleBuilder::new(2, MarketHours::NSE_EQUITY)
    }

    #[test]
    fn builds_ohlcv_and_closes_on_next_minute() {
        let mut b = builder();
        // Pre-open snapshot: baseline only.
        assert_eq!(
            b.on_tick(&tick(0, at(9, 8, 0), 100, 5_000)),
            TickOutcome::default()
        );
        assert!(b.on_tick(&tick(0, at(9, 15, 1), 101, 5_010)).traded);
        b.on_tick(&tick(0, at(9, 15, 20), 104, 5_030));
        b.on_tick(&tick(0, at(9, 15, 40), 99, 5_035));
        b.on_tick(&tick(0, at(9, 15, 59), 102, 5_050));
        let forming = *b.forming(InstrumentId(0)).unwrap();
        assert_eq!(forming.ts, at(9, 15, 0));

        let out = b.on_tick(&tick(0, at(9, 16, 2), 103, 5_060));
        let bar = out.closed.unwrap();
        assert_eq!(bar, forming);
        assert_eq!(bar.ts, at(9, 15, 0));
        assert_eq!(bar.open, Price::from_paise(10_100));
        assert_eq!(bar.high, Price::from_paise(10_400));
        assert_eq!(bar.low, Price::from_paise(9_900));
        assert_eq!(bar.close, Price::from_paise(10_200));
        assert_eq!(bar.volume, 50); // 5_050 - 5_000; pre-open volume excluded
        assert!(bar.validate().is_ok());
        assert_eq!(b.forming(InstrumentId(0)).unwrap().volume, 10);
    }

    #[test]
    fn quotes_without_trades_change_nothing() {
        let mut b = builder();
        b.on_tick(&tick(0, at(9, 8, 0), 100, 1_000));
        b.on_tick(&tick(0, at(9, 15, 5), 101, 1_010));
        // Same volume, different price (e.g. a depth update): ignored.
        let out = b.on_tick(&tick(0, at(9, 15, 6), 150, 1_010));
        assert!(!out.traded);
        assert_eq!(
            b.forming(InstrumentId(0)).unwrap().high,
            Price::from_paise(10_100)
        );
    }

    #[test]
    fn quiet_stock_closes_on_the_clock() {
        let mut b = builder();
        b.on_tick(&tick(1, at(9, 8, 0), 100, 0));
        b.on_tick(&tick(1, at(9, 20, 10), 100, 10));
        assert!(b.close_due(at(9, 21, 1)).is_empty()); // within grace
        let done = b.close_due(at(9, 21, 2));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].ts, at(9, 20, 0));
        assert!(b.forming(InstrumentId(1)).is_none());
        // A straggler for 09:20 is counted as late, not made into a candle.
        let out = b.on_tick(&tick(1, at(9, 20, 59), 100, 20));
        assert!(!out.traded);
        assert_eq!(b.late_trades(), 1);
    }

    #[test]
    fn trades_outside_the_session_are_ignored() {
        let mut b = builder();
        b.on_tick(&tick(0, at(9, 8, 0), 100, 100));
        b.on_tick(&tick(0, at(9, 10, 0), 100, 200));
        assert!(b.forming(InstrumentId(0)).is_none());
        b.on_tick(&tick(0, at(15, 40, 0), 100, 300));
        assert!(b.forming(InstrumentId(0)).is_none());
    }

    #[test]
    fn starting_mid_session_uses_the_first_trade() {
        let mut b = builder();
        let out = b.on_tick(&tick(0, at(11, 0, 30), 250, 900_000));
        assert!(out.traded);
        let bar = b.forming(InstrumentId(0)).unwrap();
        assert_eq!(bar.open, Price::from_paise(25_000));
        assert_eq!(bar.volume, 0); // earlier volume this minute is unknown
    }

    #[test]
    fn minutes_already_loaded_are_not_rebuilt() {
        let mut b = builder();
        b.mark_closed(InstrumentId(0), at(10, 0, 0));
        b.on_tick(&tick(0, at(10, 0, 30), 100, 10));
        assert!(b.forming(InstrumentId(0)).is_none());
        assert_eq!(b.late_trades(), 1);
        b.on_tick(&tick(0, at(10, 1, 0), 100, 20));
        assert_eq!(b.forming(InstrumentId(0)).unwrap().ts, at(10, 1, 0));
    }

    #[test]
    fn unknown_stock_is_ignored() {
        let mut b = builder();
        assert_eq!(
            b.on_tick(&tick(7, at(9, 30, 0), 100, 10)),
            TickOutcome::default()
        );
    }
}
