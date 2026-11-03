//! One struct per table in `deploy/questdb/schema.sql`. Field names and
//! types mirror the columns exactly; keep the two in sync.
//!
//! Text fields are `Arc<str>` so that hot-path rows (ticks, bars) can reuse
//! interned symbol and strategy names without allocating per row.
//! Money is `Decimal` and is written with exactly 2 decimal places to match
//! the `DECIMAL(p, 2)` columns.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use questdb::ingress::{Buffer, TimestampMicros};
use rust_decimal::{Decimal, RoundingStrategy};

/// Interned text for symbol-like and free-text fields.
pub type Text = Arc<str>;

/// Which kind of run produced a row. Lets backtest, paper and live share
/// tables and be compared directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Backtest,
    Paper,
    Live,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Backtest => "backtest",
            Mode::Paper => "paper",
            Mode::Live => "live",
        }
    }
}

const MONEY_SCALE: u32 = 2;

/// Rounds half away from zero and fixes the scale at 2, so `1500` is sent
/// as `1500.00` and `1428.055` as `1428.06`.
fn money(d: Decimal) -> Decimal {
    let mut r = d.round_dp_with_strategy(MONEY_SCALE, RoundingStrategy::MidpointAwayFromZero);
    r.rescale(MONEY_SCALE);
    r
}

fn micros(ts: &DateTime<Utc>) -> TimestampMicros {
    TimestampMicros::new(ts.timestamp_micros())
}

type QResult = questdb::Result<()>;

/// A 1-minute or daily candle.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    /// Candle start.
    pub ts: DateTime<Utc>,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub oi: Option<i64>,
    /// Where it came from, e.g. `dhan_hist`, `live`.
    pub source: Text,
}

impl Bar {
    fn append(&self, buf: &mut Buffer, table: &str) -> QResult {
        buf.table(table)?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .symbol("source", &*self.source)?
            .column_i32("security_id", self.security_id)?
            .column_f64("open", self.open)?
            .column_f64("high", self.high)?
            .column_f64("low", self.low)?
            .column_f64("close", self.close)?
            .column_i64("volume", self.volume)?
            .column_i64_opt("oi", self.oi)?
            .at(micros(&self.ts))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// Time we received it.
    pub ts: DateTime<Utc>,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub ltp: f64,
    pub ltq: Option<i32>,
    /// Exchange last-trade time.
    pub ltt: Option<DateTime<Utc>>,
    pub volume: Option<i64>,
    pub atp: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub bid_qty: Option<i32>,
    pub ask_qty: Option<i32>,
    pub total_buy_qty: Option<i64>,
    pub total_sell_qty: Option<i64>,
}

impl Tick {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("ticks")?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .column_i32("security_id", self.security_id)?
            .column_f64("ltp", self.ltp)?
            .column_i32_opt("ltq", self.ltq)?
            .column_ts_opt("ltt", self.ltt.as_ref().map(micros))?
            .column_i64_opt("volume", self.volume)?
            .column_f64_opt("atp", self.atp)?
            .column_f64_opt("bid", self.bid)?
            .column_f64_opt("ask", self.ask)?
            .column_i32_opt("bid_qty", self.bid_qty)?
            .column_i32_opt("ask_qty", self.ask_qty)?
            .column_i64_opt("total_buy_qty", self.total_buy_qty)?
            .column_i64_opt("total_sell_qty", self.total_sell_qty)?
            .at(micros(&self.ts))
    }
}

/// Daily snapshot of one tradable instrument.
#[derive(Debug, Clone, PartialEq)]
pub struct InstrumentRow {
    /// Snapshot date (midnight UTC of the trading day).
    pub ts: DateTime<Utc>,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub series: Option<Text>,
    pub isin: Option<Text>,
    pub name: Option<Text>,
    pub tick_size: f64,
    pub lot_size: i32,
    pub upper_circuit: Option<f64>,
    pub lower_circuit: Option<f64>,
    pub asm_gsm: bool,
    pub mtf_leverage: Option<f64>,
}

impl InstrumentRow {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("instruments")?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?;
        if let Some(series) = &self.series {
            buf.symbol("series", &**series)?;
        }
        buf.column_i32("security_id", self.security_id)?
            .column_str_opt("isin", self.isin.as_deref())?
            .column_str_opt("name", self.name.as_deref())?
            .column_f64("tick_size", self.tick_size)?
            .column_i32("lot_size", self.lot_size)?
            .column_f64_opt("upper_circuit", self.upper_circuit)?
            .column_f64_opt("lower_circuit", self.lower_circuit)?
            .column_bool("asm_gsm", self.asm_gsm)?
            .column_f64_opt("mtf_leverage", self.mtf_leverage)?
            .at(micros(&self.ts))
    }
}

/// One backtest / paper / live run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub strategy: Text,
    pub broker: Text,
    pub git_sha: Option<Text>,
    /// Strategy parameters, as JSON.
    pub params: Option<Text>,
    pub notes: Option<Text>,
}

impl RunRow {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("runs")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("strategy", &*self.strategy)?
            .symbol("broker", &*self.broker)?
            .column_str_opt("git_sha", self.git_sha.as_deref())?
            .column_str_opt("params", self.params.as_deref())?
            .column_str_opt("notes", self.notes.as_deref())?
            .at(micros(&self.ts))
    }
}

/// A strategy signal and the risk manager's decision on it.
#[derive(Debug, Clone, PartialEq)]
pub struct SignalRow {
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub strategy: Text,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    /// `BUY` / `SELL`.
    pub side: Text,
    pub entry: Option<Decimal>,
    pub stop: Option<Decimal>,
    pub target: Option<Decimal>,
    pub qty: i32,
    /// `ACCEPTED` / `REJECTED`.
    pub decision: Text,
    pub reason: Option<Text>,
}

impl SignalRow {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("signals")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("strategy", &*self.strategy)?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .symbol("side", &*self.side)?
            .symbol("decision", &*self.decision)?
            .column_i32("security_id", self.security_id)?
            .column_dec_opt("entry", self.entry.map(money).as_ref())?
            .column_dec_opt("stop", self.stop.map(money).as_ref())?
            .column_dec_opt("target", self.target.map(money).as_ref())?
            .column_i32("qty", self.qty)?
            .column_str_opt("reason", self.reason.as_deref())?
            .at(micros(&self.ts))
    }
}

/// One status change of one order.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEvent {
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub strategy: Text,
    pub correlation_id: Text,
    pub order_id: Option<Text>,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub side: Text,
    pub product: Text,
    pub order_type: Text,
    pub qty: i32,
    pub price: Option<Decimal>,
    pub trigger_price: Option<Decimal>,
    pub status: Text,
    pub filled_qty: i32,
    pub avg_price: Option<Decimal>,
    pub reason: Option<Text>,
}

impl OrderEvent {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("orders")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("strategy", &*self.strategy)?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .symbol("side", &*self.side)?
            .symbol("product", &*self.product)?
            .symbol("order_type", &*self.order_type)?
            .symbol("status", &*self.status)?
            .column_str("correlation_id", &*self.correlation_id)?
            .column_str_opt("order_id", self.order_id.as_deref())?
            .column_i32("security_id", self.security_id)?
            .column_i32("qty", self.qty)?
            .column_dec_opt("price", self.price.map(money).as_ref())?
            .column_dec_opt("trigger_price", self.trigger_price.map(money).as_ref())?
            .column_i32("filled_qty", self.filled_qty)?
            .column_dec_opt("avg_price", self.avg_price.map(money).as_ref())?
            .column_str_opt("reason", self.reason.as_deref())?
            .at(micros(&self.ts))
    }
}

/// One execution.
#[derive(Debug, Clone, PartialEq)]
pub struct FillRow {
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub strategy: Text,
    pub correlation_id: Text,
    pub order_id: Text,
    pub trade_id: Text,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub side: Text,
    pub qty: i32,
    pub price: Decimal,
    /// Price the strategy expected, to measure slippage.
    pub expected_price: Option<Decimal>,
    pub charges: Option<Decimal>,
}

impl FillRow {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("fills")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("strategy", &*self.strategy)?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .symbol("side", &*self.side)?
            .column_str("correlation_id", &*self.correlation_id)?
            .column_str("order_id", &*self.order_id)?
            .column_str("trade_id", &*self.trade_id)?
            .column_i32("security_id", self.security_id)?
            .column_i32("qty", self.qty)?
            .column_dec("price", &money(self.price))?
            .column_dec_opt("expected_price", self.expected_price.map(money).as_ref())?
            .column_dec_opt("charges", self.charges.map(money).as_ref())?
            .at(micros(&self.ts))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PositionRow {
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub strategy: Text,
    pub segment: Text,
    pub security_id: i32,
    pub symbol: Text,
    pub net_qty: i32,
    pub avg_price: Decimal,
    pub ltp: Option<f64>,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
}

impl PositionRow {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("positions")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("strategy", &*self.strategy)?
            .symbol("segment", &*self.segment)?
            .symbol("symbol", &*self.symbol)?
            .column_i32("security_id", self.security_id)?
            .column_i32("net_qty", self.net_qty)?
            .column_dec("avg_price", &money(self.avg_price))?
            .column_f64_opt("ltp", self.ltp)?
            .column_dec("realized_pnl", &money(self.realized_pnl))?
            .column_dec("unrealized_pnl", &money(self.unrealized_pnl))?
            .at(micros(&self.ts))
    }
}

/// Daily result of one strategy, net of costs.
#[derive(Debug, Clone, PartialEq)]
pub struct PnlDailyRow {
    /// Trading day (midnight UTC).
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub strategy: Text,
    pub trades: i32,
    pub wins: i32,
    pub losses: i32,
    pub gross_pnl: Decimal,
    pub charges: Decimal,
    pub net_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub turnover: Decimal,
}

impl PnlDailyRow {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("pnl_daily")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("strategy", &*self.strategy)?
            .column_i32("trades", self.trades)?
            .column_i32("wins", self.wins)?
            .column_i32("losses", self.losses)?
            .column_dec("gross_pnl", &money(self.gross_pnl))?
            .column_dec("charges", &money(self.charges))?
            .column_dec("net_pnl", &money(self.net_pnl))?
            .column_dec("max_drawdown", &money(self.max_drawdown))?
            .column_dec("turnover", &money(self.turnover))?
            .at(micros(&self.ts))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Operational event: connect/disconnect, risk halt, kill switch, error.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemEvent {
    pub ts: DateTime<Utc>,
    pub run_id: Text,
    pub mode: Mode,
    pub level: Level,
    pub component: Text,
    pub event: Text,
    pub message: Text,
}

impl SystemEvent {
    fn append(&self, buf: &mut Buffer) -> QResult {
        buf.table("system_events")?
            .symbol("run_id", &*self.run_id)?
            .symbol("mode", self.mode.as_str())?
            .symbol("level", self.level.as_str())?
            .symbol("component", &*self.component)?
            .symbol("event", &*self.event)?
            .column_str("message", &*self.message)?
            .at(micros(&self.ts))
    }
}

/// Any row the store can write.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Bar1m(Bar),
    BarDaily(Bar),
    Tick(Tick),
    Instrument(InstrumentRow),
    Run(RunRow),
    Signal(SignalRow),
    Order(OrderEvent),
    Fill(FillRow),
    Position(PositionRow),
    PnlDaily(PnlDailyRow),
    System(SystemEvent),
}

impl Record {
    pub fn table(&self) -> &'static str {
        match self {
            Record::Bar1m(_) => "bars_1m",
            Record::BarDaily(_) => "bars_daily",
            Record::Tick(_) => "ticks",
            Record::Instrument(_) => "instruments",
            Record::Run(_) => "runs",
            Record::Signal(_) => "signals",
            Record::Order(_) => "orders",
            Record::Fill(_) => "fills",
            Record::Position(_) => "positions",
            Record::PnlDaily(_) => "pnl_daily",
            Record::System(_) => "system_events",
        }
    }

    /// Appends this row to a QWP buffer.
    pub fn append_to(&self, buf: &mut Buffer) -> questdb::Result<()> {
        match self {
            Record::Bar1m(b) => b.append(buf, "bars_1m"),
            Record::BarDaily(b) => b.append(buf, "bars_daily"),
            Record::Tick(t) => t.append(buf),
            Record::Instrument(i) => i.append(buf),
            Record::Run(r) => r.append(buf),
            Record::Signal(s) => s.append(buf),
            Record::Order(o) => o.append(buf),
            Record::Fill(f) => f.append(buf),
            Record::Position(p) => p.append(buf),
            Record::PnlDaily(p) => p.append(buf),
            Record::System(e) => e.append(buf),
        }
    }
}

macro_rules! into_record {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(impl From<$ty> for Record {
            fn from(v: $ty) -> Self {
                Record::$variant(v)
            }
        })*
    };
}

into_record! {
    Tick => Tick,
    InstrumentRow => Instrument,
    RunRow => Run,
    SignalRow => Signal,
    OrderEvent => Order,
    FillRow => Fill,
    PositionRow => Position,
    PnlDailyRow => PnlDaily,
    SystemEvent => System,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_has_two_places_and_rounds_half_up() {
        assert_eq!(money(Decimal::new(1500, 0)).to_string(), "1500.00");
        assert_eq!(money(Decimal::new(1_428_055, 3)).to_string(), "1428.06");
        assert_eq!(money(Decimal::new(-1_428_055, 3)).to_string(), "-1428.06");
        assert_eq!(money(Decimal::new(14_281, 1)).to_string(), "1428.10");
    }

    #[test]
    fn rows_encode_without_a_server() {
        let ts = DateTime::from_timestamp(1_762_140_600, 0).unwrap();
        let t: Text = "x".into();
        let mut buf = Buffer::new_qwp_ws();
        let rows: Vec<Record> = vec![
            Record::Bar1m(Bar {
                ts,
                segment: "NSE_EQ".into(),
                security_id: 2885,
                symbol: "RELIANCE".into(),
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 1.5,
                volume: 10,
                oi: None,
                source: "test".into(),
            }),
            FillRow {
                ts,
                run_id: t.clone(),
                mode: Mode::Paper,
                strategy: t.clone(),
                correlation_id: t.clone(),
                order_id: t.clone(),
                trade_id: t.clone(),
                segment: t.clone(),
                security_id: 1,
                symbol: t.clone(),
                side: "BUY".into(),
                qty: 1,
                price: Decimal::new(142_805, 2),
                expected_price: None,
                charges: Some(Decimal::new(18, 0)),
            }
            .into(),
            SystemEvent {
                ts,
                run_id: t.clone(),
                mode: Mode::Live,
                level: Level::Warn,
                component: "feed".into(),
                event: "disconnected".into(),
                message: "stream ended".into(),
            }
            .into(),
        ];
        for r in &rows {
            r.append_to(&mut buf).unwrap();
        }
        assert_eq!(buf.row_count(), 3);
    }
}
