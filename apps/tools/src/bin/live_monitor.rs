//! Live market monitor: streams Dhan quotes for a universe, builds 1-minute
//! candles, and shows indicators two ways:
//!
//! * **live**: as if the forming candle closed now; moves with every trade.
//!   For watching only: it can change until the minute ends.
//! * **closed**: the value after the last finished candle; what a strategy
//!   and a backtest see.
//!
//! Ticks go to QuestDB `ticks` and finished candles to `bars_1m`
//! (`source = 'live'`; a later history download replaces them with Dhan's
//! official candles).
//!
//! ```bash
//! set -a; source .env; set +a
//! scripts/questdb.sh start
//! cargo run --release -p tools --bin live-monitor
//! cargo run --release -p tools --bin live-monitor -- \
//!     --indicators "ema(9),ema(20),bb(20,2).upper" --show RELIANCE,TCS --minutes 30
//! ```
//!
//! Indicators are warmed up from `bars_1m` history, then today's earlier
//! candles are fetched from Dhan, so values are right from the first tick
//! even when started mid-day.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use clap::Parser;
use dhan::feed::packet::{Full, Quote};
use dhan::feed::{FeedEvent, FeedMode, FeedPacket, MarketFeed, MarketFeedConfig};
use dhan::token::connect_from_env;
use dhan::{
    Candle, CandleInterval, DhanClient, ExchangeSegment, InstrumentId as DhanId,
    IntradayCandlesRequest, SecurityId,
};
use indicators::{BankBuilder, Handle, IndicatorBank, Registry};
use store::query::{BarTable, QueryClient};
use store::rows::{Bar as BarRow, DEPTH_LEVELS, DepthLevel, IndicatorRow, Tick as TickRow};
use store::{Record, Store, StoreConfig};
use trading_core::{Bar, CandleBuilder, InstrumentId, MarketHours, Price, Tick, Timestamp};

type Error = Box<dyn std::error::Error>;

const SEGMENT: &str = "NSE_EQ";
/// Dhan sends trade times as IST clock time written as if it were UTC.
const DHAN_LTT_OFFSET_SECS: i64 = 5 * 3600 + 1800;

#[derive(Debug, Parser)]
#[command(about = "Live candles and live/closed indicators from the Dhan feed")]
struct Args {
    /// `symbol,security_id,tick_paise,lot_size` per line, after a header.
    #[arg(long, default_value = "data/universe.csv")]
    universe: String,
    /// Indicator specs, comma-separated (commas inside brackets are kept).
    #[arg(
        long,
        default_value = "ema(9),ema(20),vwap,rsi(14),orb(15).high,orb(15).low"
    )]
    indicators: String,
    /// Only print these symbols (all are still recorded).
    #[arg(long, value_delimiter = ',')]
    show: Vec<String>,
    /// Calendar days of history used to warm up indicators.
    #[arg(long, default_value_t = 10)]
    warmup_days: i64,
    /// Seconds between live tables.
    #[arg(long, default_value_t = 15)]
    print_every: u64,
    /// Stop after this many minutes (default: run until Ctrl-C).
    #[arg(long)]
    minutes: Option<u64>,
    /// Do not write ticks or candles to QuestDB.
    #[arg(long)]
    no_store: bool,
    #[arg(long, default_value = "127.0.0.1:9000")]
    qdb: String,
}

/// Dhan sends prices as 32-bit floats; `1354.6` arrives as `1354.5999755`.
/// Rounded to the paisa so stored prices read as the exchange quoted them.
fn paise_f64(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

/// One feed update, from either quote or full (quote + depth) packets.
struct Snapshot {
    security_id: u32,
    ltp: f32,
    ltq: u16,
    ltt: u32,
    atp: f32,
    volume: u32,
    total_buy_qty: u32,
    total_sell_qty: u32,
    open: f32,
    high: f32,
    low: f32,
    depth: Option<[DepthLevel; DEPTH_LEVELS]>,
}

impl Snapshot {
    fn from_quote(q: &Quote) -> Self {
        Self {
            security_id: q.header.security_id,
            ltp: q.ltp,
            ltq: q.ltq,
            ltt: q.ltt,
            atp: q.atp,
            volume: q.volume,
            total_buy_qty: q.total_buy_qty,
            total_sell_qty: q.total_sell_qty,
            open: q.open,
            high: q.high,
            low: q.low,
            depth: None,
        }
    }

    fn from_full(f: &Full) -> Self {
        let level = |i: usize| {
            f.depth
                .get(i)
                .map_or_else(DepthLevel::default, |d| DepthLevel {
                    bid_px: paise_f64(d.bid_price),
                    bid_qty: i32::try_from(d.bid_qty).unwrap_or(i32::MAX),
                    bid_orders: i32::from(d.bid_orders),
                    ask_px: paise_f64(d.ask_price),
                    ask_qty: i32::try_from(d.ask_qty).unwrap_or(i32::MAX),
                    ask_orders: i32::from(d.ask_orders),
                })
        };
        Self {
            security_id: f.header.security_id,
            ltp: f.ltp,
            ltq: f.ltq,
            ltt: f.ltt,
            atp: f.atp,
            volume: f.volume,
            total_buy_qty: f.total_buy_qty,
            total_sell_qty: f.total_sell_qty,
            open: f.open,
            high: f.high,
            low: f.low,
            depth: Some(std::array::from_fn(level)),
        }
    }
}

struct Stock {
    symbol: Arc<str>,
    security_id: u32,
    ltp: Option<Price>,
}

struct Monitor {
    stocks: Vec<Stock>,
    by_security: HashMap<u32, InstrumentId>,
    specs: Vec<String>,
    handles: Vec<Handle>,
    bank: IndicatorBank,
    candles: CandleBuilder,
    store: Option<Store>,
    show: Vec<bool>,
    ticks: u64,
    live_bars: u64,
    rejected: u64,
}

/// Splits `ema(9),bb(20,2).upper` into specs, keeping bracketed commas.
fn split_specs(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn load_universe(path: &str) -> Result<Vec<Stock>, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut stocks = Vec::new();
    for (n, line) in text.lines().enumerate().skip(1) {
        let mut cols = line.split(',').map(str::trim);
        let (Some(symbol), Some(id)) = (cols.next(), cols.next()) else {
            continue;
        };
        if symbol.is_empty() {
            continue;
        }
        let security_id = id
            .parse()
            .map_err(|_| format!("{path} line {}: bad security id {id:?}", n + 1))?;
        stocks.push(Stock {
            symbol: Arc::from(symbol),
            security_id,
            ltp: None,
        });
    }
    if stocks.is_empty() {
        return Err(format!("{path}: no stocks").into());
    }
    Ok(stocks)
}

fn now() -> Timestamp {
    Timestamp::from_datetime(&Utc::now())
}

fn hhmm(ts: Timestamp) -> String {
    ts.to_ist()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

fn fmt_value(v: Option<f64>) -> String {
    v.map_or_else(|| "-".into(), |v| format!("{v:.2}"))
}

fn row_to_bar(instrument: InstrumentId, row: &BarRow) -> Option<Bar> {
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

fn candle_to_bar(instrument: InstrumentId, c: &Candle) -> Option<Bar> {
    let bar = Bar {
        instrument,
        ts: Timestamp::from_secs(c.timestamp),
        open: Price::from_decimal(c.open)?,
        high: Price::from_decimal(c.high)?,
        low: Price::from_decimal(c.low)?,
        close: Price::from_decimal(c.close)?,
        volume: u64::try_from(c.volume).ok()?,
    };
    bar.validate().ok().map(|()| bar)
}

impl Monitor {
    fn bar_row(&self, bar: &Bar, source: &str) -> Option<BarRow> {
        let stock = self.stocks.get(bar.instrument.index())?;
        Some(BarRow {
            ts: bar.ts.to_utc()?,
            segment: Arc::from(SEGMENT),
            security_id: i32::try_from(stock.security_id).ok()?,
            symbol: stock.symbol.clone(),
            open: bar.open.as_f64(),
            high: bar.high.as_f64(),
            low: bar.low.as_f64(),
            close: bar.close.as_f64(),
            volume: i64::try_from(bar.volume).ok()?,
            oi: None,
            source: Arc::from(source),
        })
    }

    fn store_bar(&self, bar: &Bar, source: &str) {
        if let (Some(store), Some(row)) = (&self.store, self.bar_row(bar, source))
            && let Err(e) = store.write(Record::Bar1m(row))
        {
            eprintln!("store: {e}");
        }
    }

    /// History before today, from QuestDB.
    fn warm_up(&mut self, qdb: &str, days: i64) -> Result<u64, Error> {
        let mut client = QueryClient::connect(&StoreConfig {
            addr: qdb.to_owned(),
            ..StoreConfig::default()
        })?;
        let today = now()
            .ist_date()
            .and_then(|d| Timestamp::from_ist(d, NaiveTime::MIN))
            .and_then(Timestamp::to_utc)
            .ok_or("bad clock")?;
        let from = today - TimeDelta::days(days);
        let mut fed = 0;
        let ids: Vec<(InstrumentId, i32)> = self
            .stocks
            .iter()
            .enumerate()
            .map(|(i, st)| {
                Ok((
                    InstrumentId(u32::try_from(i)?),
                    i32::try_from(st.security_id)?,
                ))
            })
            .collect::<Result<_, Error>>()?;
        for (instrument, sid) in ids {
            for row in client.bars(BarTable::OneMinute, sid, from, today)? {
                if let Some(bar) = row_to_bar(instrument, &row)
                    && self.bank.update(&bar).is_ok()
                {
                    fed += 1;
                }
            }
        }
        Ok(fed)
    }

    /// Today's finished candles so far, from Dhan, so a mid-day start
    /// still has correct VWAP, ORB and moving averages.
    async fn catch_up(&mut self, client: &DhanClient) -> Result<u64, Error> {
        let now = now();
        let current_minute = now.floor_minute();
        let Some(date) = now.ist_date() else {
            return Ok(0);
        };
        let from = date.and_time(NaiveTime::from_hms_opt(9, 0, 0).ok_or("bad time")?);
        let to = date.and_time(NaiveTime::from_hms_opt(15, 30, 0).ok_or("bad time")?);
        let mut fed = 0;
        let ids: Vec<(InstrumentId, u32, Arc<str>)> = self
            .stocks
            .iter()
            .enumerate()
            .map(|(i, st)| {
                Ok((
                    InstrumentId(u32::try_from(i)?),
                    st.security_id,
                    st.symbol.clone(),
                ))
            })
            .collect::<Result<_, Error>>()?;
        for (instrument, security_id, symbol) in ids {
            let request = IntradayCandlesRequest::equity(
                ExchangeSegment::NseEq,
                SecurityId(security_id),
                CandleInterval::Min1,
                from,
                to,
            );
            let candles = match client.intraday_candles(&request).await {
                Ok(c) => c,
                // Before the open there is nothing yet.
                Err(e) if e.kind() == dhan::ErrorKind::Data => continue,
                Err(e) => return Err(format!("{symbol}: {e}").into()),
            };
            for c in &candles {
                let Some(bar) = candle_to_bar(instrument, c) else {
                    continue;
                };
                // The current minute is still forming; the feed builds it.
                if bar.ts >= current_minute || self.bank.update(&bar).is_err() {
                    continue;
                }
                self.candles.mark_closed(instrument, bar.ts);
                self.store_bar(&bar, "dhan_hist");
                fed += 1;
            }
        }
        Ok(fed)
    }

    fn on_quote(&mut self, q: &Snapshot) {
        let Some(&instrument) = self.by_security.get(&q.security_id) else {
            return;
        };
        self.ticks += 1;
        // One receive time for the tick row and its live indicator rows, so
        // they join on (ts, security_id).
        let received = Utc::now();
        let ltp = Price::from_rupees_f64(paise_f64(q.ltp));
        let ltt =
            (q.ltt > 0).then(|| Timestamp::from_secs(i64::from(q.ltt) - DHAN_LTT_OFFSET_SECS));
        self.store_tick(instrument, q, received, ltt);

        if let (Some(ltp), Some(ts)) = (ltp, ltt) {
            if let Some(stock) = self.stocks.get_mut(instrument.index()) {
                stock.ltp = Some(ltp);
            }
            let tick = Tick {
                instrument,
                ts,
                ltp,
                ltq: u32::from(q.ltq),
                volume: u64::from(q.volume),
                bid: q.depth.and_then(|d| Price::from_rupees_f64(d[0].bid_px)),
                ask: q.depth.and_then(|d| Price::from_rupees_f64(d[0].ask_px)),
                day_open: Price::from_rupees_f64(f64::from(q.open)),
                day_high: Price::from_rupees_f64(f64::from(q.high)),
                day_low: Price::from_rupees_f64(f64::from(q.low)),
            };
            if let Some(bar) = self.candles.on_tick(&tick).closed {
                self.on_closed(bar);
            }
        }
        self.store_live_indicators(instrument, received);
    }

    fn store_tick(
        &self,
        instrument: InstrumentId,
        q: &Snapshot,
        received: DateTime<Utc>,
        ltt: Option<Timestamp>,
    ) {
        let (Some(store), Some(stock)) = (&self.store, self.stocks.get(instrument.index())) else {
            return;
        };
        let best = q.depth.map(|d| d[0]);
        let row = TickRow {
            ts: received,
            segment: Arc::from(SEGMENT),
            security_id: i32::try_from(stock.security_id).unwrap_or_default(),
            symbol: stock.symbol.clone(),
            ltp: paise_f64(q.ltp),
            ltq: Some(i32::from(q.ltq)),
            ltt: ltt.and_then(Timestamp::to_utc),
            volume: Some(i64::from(q.volume)),
            atp: Some(paise_f64(q.atp)),
            bid: best.map(|l| l.bid_px),
            ask: best.map(|l| l.ask_px),
            bid_qty: best.map(|l| l.bid_qty),
            ask_qty: best.map(|l| l.ask_qty),
            total_buy_qty: Some(i64::from(q.total_buy_qty)),
            total_sell_qty: Some(i64::from(q.total_sell_qty)),
            depth: q.depth,
        };
        if let Err(e) = store.write(Record::Tick(row)) {
            eprintln!("store: {e}");
        }
    }

    /// Indicator rows for one stock: `live` previews the forming candle
    /// (or repeats the closed value if it has not traded this minute).
    fn indicator_rows(
        &self,
        instrument: InstrumentId,
        ts: DateTime<Utc>,
        live: bool,
    ) -> Vec<IndicatorRow> {
        let Some(stock) = self.stocks.get(instrument.index()) else {
            return Vec::new();
        };
        let preview = live
            .then(|| self.candles.forming(instrument))
            .flatten()
            .and_then(|bar| self.bank.preview(bar).ok());
        let security_id = i32::try_from(stock.security_id).unwrap_or_default();
        self.specs
            .iter()
            .zip(&self.handles)
            .map(|(spec, h)| IndicatorRow {
                ts,
                segment: Arc::from(SEGMENT),
                security_id,
                symbol: stock.symbol.clone(),
                indicator: Arc::from(spec.as_str()),
                value: preview
                    .as_ref()
                    .map_or_else(|| self.bank.get(instrument, *h), |p| p.get(*h)),
            })
            .collect()
    }

    fn store_live_indicators(&self, instrument: InstrumentId, received: DateTime<Utc>) {
        let Some(store) = &self.store else { return };
        for row in self.indicator_rows(instrument, received, true) {
            if let Err(e) = store.write(Record::IndicatorLive(row)) {
                eprintln!("store: {e}");
            }
        }
    }

    fn store_closed_indicators(&self, bar: &Bar) {
        let (Some(store), Some(ts)) = (&self.store, bar.ts.to_utc()) else {
            return;
        };
        for row in self.indicator_rows(bar.instrument, ts, false) {
            if let Err(e) = store.write(Record::Indicator1m(row)) {
                eprintln!("store: {e}");
            }
        }
    }

    fn on_closed(&mut self, bar: Bar) {
        if bar.validate().is_err() || self.bank.update(&bar).is_err() {
            self.rejected += 1;
            return;
        }
        self.live_bars += 1;
        self.store_bar(&bar, "live");
        self.store_closed_indicators(&bar);
        if !self
            .show
            .get(bar.instrument.index())
            .copied()
            .unwrap_or(false)
        {
            return;
        }
        let symbol = self
            .stocks
            .get(bar.instrument.index())
            .map_or("?", |s| &s.symbol);
        let values: Vec<String> = self
            .specs
            .iter()
            .zip(&self.handles)
            .map(|(spec, h)| format!("{spec}={}", fmt_value(self.bank.get(bar.instrument, *h))))
            .collect();
        println!(
            "closed {} {symbol:<11} o {} h {} l {} c {} v {} | {}",
            hhmm(bar.ts),
            bar.open,
            bar.high,
            bar.low,
            bar.close,
            bar.volume,
            values.join(" ")
        );
    }

    fn print_table(&self) {
        let mut header = format!("\n{} IST  {:<11} {:>9}", hhmm(now()), "symbol", "ltp");
        for spec in &self.specs {
            header.push_str(&format!(" {:>21}", format!("{spec} live/closed")));
        }
        println!("{header}");
        for (i, stock) in self.stocks.iter().enumerate() {
            if !self.show.get(i).copied().unwrap_or(false) {
                continue;
            }
            let instrument = InstrumentId(u32::try_from(i).unwrap_or(u32::MAX));
            let preview = self
                .candles
                .forming(instrument)
                .and_then(|bar| self.bank.preview(bar).ok());
            let ltp = stock.ltp.map_or_else(|| "-".into(), |p| p.to_string());
            let mut line = format!("           {:<11} {ltp:>9}", stock.symbol);
            for h in &self.handles {
                let closed = self.bank.get(instrument, *h);
                // No trade yet this minute: live equals closed.
                let live = preview.as_ref().map_or(closed, |p| p.get(*h));
                line.push_str(&format!(
                    " {:>21}",
                    format!("{}/{}", fmt_value(live), fmt_value(closed))
                ));
            }
            println!("{line}");
        }
        println!(
            "           ticks {}  live candles {}  late trades {}  rejected {}",
            self.ticks,
            self.live_bars,
            self.candles.late_trades(),
            self.rejected
        );
    }
}

async fn run(args: Args) -> Result<(), Error> {
    let stocks = load_universe(&args.universe)?;
    let specs = split_specs(&args.indicators);
    let mut builder = BankBuilder::new(Registry::builtin());
    let handles = specs
        .iter()
        .map(|s| builder.require(s))
        .collect::<Result<Vec<_>, _>>()?;
    let bank = builder.build(stocks.len())?;

    let wanted: Vec<String> = args
        .show
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .collect();
    let show = stocks
        .iter()
        .map(|s| wanted.is_empty() || wanted.iter().any(|w| **w == *s.symbol))
        .collect();
    let by_security = stocks
        .iter()
        .enumerate()
        .filter_map(|(i, s)| Some((s.security_id, InstrumentId(u32::try_from(i).ok()?))))
        .collect();
    let store = if args.no_store {
        None
    } else {
        Some(Store::start(StoreConfig {
            addr: args.qdb.clone(),
            ..StoreConfig::default()
        })?)
    };
    let mut m = Monitor {
        candles: CandleBuilder::new(stocks.len(), MarketHours::NSE_EQUITY),
        stocks,
        by_security,
        specs,
        handles,
        bank,
        store,
        show,
        ticks: 0,
        live_bars: 0,
        rejected: 0,
    };

    let warmed = m.warm_up(&args.qdb, args.warmup_days)?;
    println!(
        "warmed up on {warmed} history bars ({} days)",
        args.warmup_days
    );
    let client = connect_from_env().await?;
    let caught = m.catch_up(&client).await?;
    println!("caught up on {caught} candles from earlier today");

    let (feed, mut events) = MarketFeed::spawn(MarketFeedConfig::from_client(&client))?;
    let ids: Vec<DhanId> = m
        .stocks
        .iter()
        .map(|s| DhanId::nse_eq(s.security_id))
        .collect();
    // Full: the quote plus 5-level depth.
    feed.subscribe(FeedMode::Full, ids)?;
    println!("streaming {} stocks; Ctrl-C to stop", m.stocks.len());

    let deadline =
        tokio::time::sleep(args.minutes.map_or(Duration::from_secs(u64::MAX / 4), |n| {
            Duration::from_secs(n * 60)
        }));
    tokio::pin!(deadline);
    let mut second = tokio::time::interval(Duration::from_secs(1));
    let print_every = args.print_every.max(1);
    let mut elapsed = 0u64;
    loop {
        tokio::select! {
            () = &mut deadline => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = second.tick() => {
                for bar in m.candles.close_due(now()) {
                    m.on_closed(bar);
                }
                elapsed += 1;
                if elapsed.is_multiple_of(print_every) {
                    m.print_table();
                }
            }
            ev = events.recv() => match ev {
                None => break,
                Some(FeedEvent::Packet(FeedPacket::Full(f))) => m.on_quote(&Snapshot::from_full(&f)),
                Some(FeedEvent::Packet(FeedPacket::Quote(q))) => m.on_quote(&Snapshot::from_quote(&q)),
                Some(FeedEvent::Connected) => println!("feed connected"),
                Some(FeedEvent::Disconnected { reason }) => println!("feed disconnected: {reason}"),
                Some(FeedEvent::ParseError(e)) => println!("feed parse error: {e}"),
                Some(FeedEvent::Packet(_)) => {}
            },
        }
    }
    feed.shutdown().await;
    m.print_table();
    if let Some(store) = m.store.take() {
        store.flush()?;
        let stats = store.stats();
        store.shutdown()?;
        println!("store: {stats:?}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn float_prices_round_to_the_paisa() {
        assert_eq!(paise_f64(1354.6), 1354.6);
        assert_eq!(paise_f64(223.53), 223.53);
        assert_eq!(paise_f64(12_992.05), 12_992.05);
    }

    #[test]
    fn specs_keep_bracketed_commas() {
        assert_eq!(
            split_specs("ema(9), bb(20,2).upper ,vwap,"),
            ["ema(9)", "bb(20,2).upper", "vwap"]
        );
    }

    #[test]
    fn dhan_trade_time_is_shifted_to_utc() {
        // 15:59:50 IST yesterday, as Dhan sends it.
        let raw: i64 = 1795017590;
        let ts = Timestamp::from_secs(raw - DHAN_LTT_OFFSET_SECS);
        let ist = ts.to_ist().unwrap();
        assert_eq!(ist.format("%H:%M:%S").to_string(), "15:59:50");
    }
}
