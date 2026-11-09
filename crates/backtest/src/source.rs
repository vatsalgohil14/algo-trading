//! Where the bars come from.

use std::collections::VecDeque;

use chrono::{Datelike, NaiveDate, TimeDelta, Utc};
use sim::synthetic::{StockProfile, SyntheticConfig, SyntheticMarket, weekdays};
use store::StoreConfig;
use store::query::{BarTable, QueryClient};
use trading_core::{Bar, Exchange, InstrumentId, MarketHours, Price, Timestamp, Universe};

use crate::BacktestError;

/// Provides whole sessions in date order. Bars within a session are ordered
/// by time, then by instrument.
pub trait BarSource {
    fn next_session(&mut self) -> Result<Option<(NaiveDate, Vec<Bar>)>, BacktestError>;
}

/// Generated data.
#[derive(Debug)]
pub struct SyntheticSource {
    market: SyntheticMarket,
    dates: Vec<NaiveDate>,
    next: usize,
}

impl SyntheticSource {
    pub fn new(
        profiles: Vec<StockProfile>,
        seed: u64,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Self, BacktestError> {
        let config = SyntheticConfig {
            seed,
            ..SyntheticConfig::default()
        };
        Ok(Self {
            market: SyntheticMarket::new(config, profiles)?,
            dates: weekdays(from, to).collect(),
            next: 0,
        })
    }
}

impl BarSource for SyntheticSource {
    fn next_session(&mut self) -> Result<Option<(NaiveDate, Vec<Bar>)>, BacktestError> {
        let Some(date) = self.dates.get(self.next).copied() else {
            return Ok(None);
        };
        self.next += 1;
        Ok(Some((date, self.market.session(date)?)))
    }
}

/// Reads the universe file: a header line, then
/// `symbol,security_id,tick_paise,lot_size` per stock. Returns the universe
/// and each stock's security id, in instrument order.
pub fn load_universe_file(path: &str) -> Result<(Universe, Vec<i32>), BacktestError> {
    let bad = |why: String| BacktestError::File {
        path: path.to_owned(),
        why,
    };
    let text = std::fs::read_to_string(path).map_err(|e| bad(e.to_string()))?;
    let mut universe = Universe::new();
    let mut ids = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let [symbol, security_id, tick, lot] = fields[..] else {
            return Err(bad(format!("line {}: expected 4 fields", n + 1)));
        };
        let number = |what: &str, v: &str| -> Result<i64, BacktestError> {
            v.parse()
                .map_err(|_| bad(format!("line {}: bad {what} {v:?}", n + 1)))
        };
        let security_id = i32::try_from(number("security id", security_id)?)
            .map_err(|_| bad(format!("line {}: security id out of range", n + 1)))?;
        let lot = u32::try_from(number("lot size", lot)?)
            .map_err(|_| bad(format!("line {}: lot size out of range", n + 1)))?;
        universe.add(
            Exchange::Nse,
            symbol,
            security_id.unsigned_abs(),
            Price::from_paise(number("tick", tick)?),
            lot,
        )?;
        ids.push(security_id);
    }
    if ids.is_empty() {
        return Err(bad("no stocks listed".into()));
    }
    Ok((universe, ids))
}

fn to_bar(instrument: InstrumentId, row: &store::rows::Bar) -> Option<Bar> {
    let bar = Bar {
        instrument,
        ts: Timestamp::from_datetime(&row.ts),
        open: Price::from_rupees_f64(row.open)?,
        high: Price::from_rupees_f64(row.high)?,
        low: Price::from_rupees_f64(row.low)?,
        close: Price::from_rupees_f64(row.close)?,
        volume: u64::try_from(row.volume).ok()?,
    };
    bar.validate().ok().map(|()| bar)
}

/// Real bars from QuestDB, read a month at a time to limit memory.
pub struct QuestDbSource {
    client: QueryClient,
    security_ids: Vec<i32>,
    hours: MarketHours,
    cursor: NaiveDate,
    to: NaiveDate,
    buffer: VecDeque<(NaiveDate, Vec<Bar>)>,
    skipped: u64,
}

impl std::fmt::Debug for QuestDbSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestDbSource")
            .field("cursor", &self.cursor)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

impl QuestDbSource {
    pub fn connect(
        addr: &str,
        security_ids: Vec<i32>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Self, BacktestError> {
        let config = StoreConfig {
            addr: addr.to_owned(),
            ..StoreConfig::default()
        };
        Ok(Self {
            client: QueryClient::connect(&config)?,
            security_ids,
            hours: MarketHours::NSE_EQUITY,
            cursor: from,
            to,
            buffer: VecDeque::new(),
            skipped: 0,
        })
    }

    /// Rows dropped as invalid or outside the session.
    pub fn skipped_rows(&self) -> u64 {
        self.skipped
    }

    fn load_next_month(&mut self) -> Result<(), BacktestError> {
        let start = self.cursor;
        let next_month = NaiveDate::from_ymd_opt(
            if start.month() == 12 {
                start.year() + 1
            } else {
                start.year()
            },
            start.month() % 12 + 1,
            1,
        )
        .ok_or_else(|| BacktestError::Config(format!("bad date after {start}")))?;
        let end = next_month.min(self.to + TimeDelta::days(1));
        self.cursor = end;

        let instant = |d: NaiveDate| {
            self.hours
                .open_at(d)
                .and_then(|t| t.plus_minutes(-12 * 60).to_utc())
                .ok_or_else(|| BacktestError::Config(format!("bad date {d}")))
        };
        let (from, to): (chrono::DateTime<Utc>, chrono::DateTime<Utc>) =
            (instant(start)?, instant(end)?);

        let mut bars = Vec::new();
        for (i, sid) in self.security_ids.iter().enumerate() {
            let id = InstrumentId(i as u32);
            let rows = self.client.bars(BarTable::OneMinute, *sid, from, to)?;
            for row in &rows {
                match to_bar(id, row).filter(|b| self.hours.is_open(b.ts)) {
                    Some(bar) => bars.push(bar),
                    None => self.skipped += 1,
                }
            }
        }
        bars.sort_by_key(|b| (b.ts, b.instrument));

        let mut sessions: Vec<(NaiveDate, Vec<Bar>)> = Vec::new();
        for bar in bars {
            let Some(date) = bar.ts.ist_date() else {
                continue;
            };
            match sessions.last_mut() {
                Some((d, day)) if *d == date => day.push(bar),
                _ => sessions.push((date, vec![bar])),
            }
        }
        self.buffer.extend(sessions);
        Ok(())
    }
}

impl BarSource for QuestDbSource {
    fn next_session(&mut self) -> Result<Option<(NaiveDate, Vec<Bar>)>, BacktestError> {
        while self.buffer.is_empty() && self.cursor <= self.to {
            self.load_next_month()?;
        }
        Ok(self.buffer.pop_front())
    }
}
