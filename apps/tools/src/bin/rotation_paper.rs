//! Paper trades the monthly rotation and monitors it every day.
//!
//! The strategy: momentum + low volatility on the top 500 NSE stocks,
//! hold 30, rebalance on the close of the first trading day of each month,
//! trade at the next open, keep held stocks while they rank in the top 60.
//! Same code as the backtest (`rotation` crate). **It places no orders**;
//! the pending orders it prints are what you would place for real.
//!
//! ```bash
//! scripts/questdb.sh start
//! cargo run --release -p tools --bin rotation-paper -- init --capital 1000000
//! scripts/rotation_daily.sh          # each evening: update bars_daily, then `run`
//! cargo run --release -p tools --bin rotation-paper -- run
//! ```
//!
//! State lives in `data/paper/<name>/`: `state.json` (the account),
//! `trades.csv`, `equity.csv` (one row a day, with the Nifty ETF) and
//! `report.txt` (the latest report). `run` processes every new trading day
//! since the last run, so missing an evening loses nothing.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{Datelike, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use rotation::paper::{Account, DayLog, Event, Progress};
use rotation::{Panel, RotationConfig, Schedule, Side, build, ranking};
use tools::daily;

type Error = Box<dyn std::error::Error>;

/// Benchmark ETF (source `dhan_daily_fund`).
const BENCHMARK: &str = "NIFTYBEES";
/// Days of data before the first day processed: 14 months of signal plus
/// a year of unbroken history for the universe, with room for holidays.
const WARMUP_DAYS: i64 = 800;
/// A held stock falling this much in a day gets an alert (no automatic
/// sale: stops tested worse).
const BIG_FALL: f64 = -0.10;
/// Data older than this many calendar days gets a warning.
const STALE_DAYS: i64 = 4;

#[derive(Debug, Parser)]
#[command(about = "Paper trade and monitor the monthly rotation (places no orders)")]
struct Args {
    #[command(subcommand)]
    command: Command,
    /// Account name: state in data/paper/<name>/.
    #[arg(long, global = true, default_value = "default")]
    name: String,
    #[arg(long, global = true, default_value = "127.0.0.1:9000")]
    qdb: String,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a paper account, then run it to the latest day.
    Init {
        /// Rupees.
        #[arg(long)]
        capital: f64,
        #[arg(long, default_value = "momentum_low_vol")]
        strategy: String,
        #[arg(long, default_value = "{}")]
        params: String,
        #[arg(long, default_value_t = 30)]
        hold: usize,
        /// Keep held stocks while they rank within this many.
        #[arg(long, default_value_t = 60)]
        exit_rank: usize,
        /// First day: its close is the first rebalance. Default: the latest
        /// day in bars_daily. An earlier day replays history from there.
        #[arg(long)]
        start: Option<NaiveDate>,
        /// Replace an existing account of this name.
        #[arg(long)]
        force: bool,
    },
    /// Process new trading days and print the report.
    Run,
}

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new(name: &str) -> Result<Self, Error> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("bad account name {name:?}").into());
        }
        Ok(Self {
            dir: Path::new("data/paper").join(name),
        })
    }
    fn state(&self) -> PathBuf {
        self.dir.join("state.json")
    }
    fn load(&self) -> Result<Account, Error> {
        let path = self.state();
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "{}: {e}; start an account with `rotation-paper init --capital <rupees>`",
                path.display()
            )
        })?;
        Ok(serde_json::from_str(&text)?)
    }
    /// Writes to a temporary file first, so a crash never leaves half a state.
    fn save(&self, a: &Account) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("state.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(a)?)?;
        std::fs::rename(&tmp, self.state())?;
        Ok(())
    }
    fn append(&self, file: &str, header: &str, rows: &str) -> Result<(), Error> {
        if rows.is_empty() {
            return Ok(());
        }
        let path = self.dir.join(file);
        let new = !path.exists();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        if new {
            writeln!(f, "{header}")?;
        }
        f.write_all(rows.as_bytes())?;
        Ok(())
    }
}

fn lakh(x: f64) -> String {
    format!("Rs {:.2} L", x / 1e5)
}

fn pct(x: f64) -> String {
    format!("{:+.1}%", x * 100.0)
}

/// Bars from WARMUP_DAYS before `first` up to today.
async fn load(qdb: &str, first: NaiveDate) -> Result<Panel, Error> {
    daily::stocks(qdb, first - chrono::TimeDelta::days(WARMUP_DAYS), None).await
}

async fn init(args: &Args, files: &Files, cmd: &Command) -> Result<(), Error> {
    let Command::Init {
        capital,
        strategy,
        params,
        hold,
        exit_rank,
        start,
        force,
    } = cmd
    else {
        return Ok(());
    };
    if files.state().exists() && !force {
        return Err(format!(
            "{} exists; use `run`, or `init --force` to replace it",
            files.state().display()
        )
        .into());
    }
    let params: serde_json::Value = serde_json::from_str(params)?;
    build(strategy, params.clone())?; // check the name and settings now
    let config = RotationConfig {
        hold: *hold,
        exit_rank: Some(*exit_rank),
        ..RotationConfig::recommended()
    };
    let start = match start {
        Some(d) => *d,
        None => {
            let today = Utc::now().date_naive();
            let p = daily::stocks(&args.qdb, today - chrono::TimeDelta::days(30), None).await?;
            *p.dates()
                .last()
                .ok_or("bars_daily has no recent data; run dhan-daily first")?
        }
    };
    let account = Account::new(strategy, params, config, *capital, start)?;
    files.save(&account)?;
    // Fresh account: old logs belong to the replaced one.
    for f in ["trades.csv", "equity.csv", "events.log", "report.txt"] {
        let _ = std::fs::remove_file(files.dir.join(f));
    }
    println!(
        "Started paper account '{}' on {start} with {}.",
        args.name,
        lakh(*capital)
    );
    run(args, files).await
}

async fn run(args: &Args, files: &Files) -> Result<(), Error> {
    let mut account = files.load()?;
    let ranker = build(&account.strategy, account.params.clone())?;
    let p = load(&args.qdb, account.last_day.unwrap_or(account.start)).await?;
    if p.dates().is_empty() {
        return Err("no data in bars_daily; run dhan-daily first".into());
    }
    let bench: HashMap<NaiveDate, f64> = daily::fund_closes(
        &args.qdb,
        BENCHMARK,
        account.start - chrono::TimeDelta::days(10),
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .collect();
    let before = account.last_day.map(|_| account.equity());
    let progress = account.advance(&p, ranker.as_ref())?;
    write_logs(files, &progress, &bench)?;
    files.save(&account)?;
    let text = report(
        &args.name,
        &account,
        &p,
        ranker.as_ref(),
        &progress,
        &bench,
        before,
    );
    std::fs::write(files.dir.join("report.txt"), &text)?;
    print!("{text}");
    Ok(())
}

fn side(s: Side) -> &'static str {
    match s {
        Side::Buy => "BUY",
        Side::Sell => "SELL",
    }
}

fn describe(e: &Event) -> String {
    match e {
        Event::Filled(f) => format!(
            "{} {} {} at {:.2} = {} (costs Rs {:.0})",
            side(f.side),
            f.shares,
            f.symbol,
            f.price,
            lakh(f.value),
            f.cost
        ),
        Event::Adjusted { symbol, factor } => {
            format!("{symbol}: price history adjusted (split or bonus), shares x{factor:.3}")
        }
        Event::NotFilled {
            symbol,
            side: s,
            reason,
        } => format!("{} {symbol} not filled: {reason}", side(*s)),
        Event::Rebalance { targets } => format!("rebalance decided: {} targets", targets.len()),
        Event::GaveUp { symbols } => format!(
            "gave up on {} after several days without trading; retried at the next rebalance",
            symbols.join(", ")
        ),
    }
}

fn write_logs(
    files: &Files,
    progress: &Progress,
    bench: &HashMap<NaiveDate, f64>,
) -> Result<(), Error> {
    let (mut trades, mut equity, mut events) = (String::new(), String::new(), String::new());
    for e in &progress.adjustments {
        writeln!(events, "{} {}", Utc::now().date_naive(), describe(e))?;
    }
    for DayLog {
        date,
        cash,
        equity: eq,
        events: day,
    } in &progress.days
    {
        let b = bench
            .get(date)
            .map(|x| format!("{x:.2}"))
            .unwrap_or_default();
        writeln!(equity, "{date},{cash:.2},{eq:.2},{b}")?;
        for e in day {
            writeln!(events, "{date} {}", describe(e))?;
            if let Event::Filled(f) = e {
                writeln!(
                    trades,
                    "{},{},{},{},{:.2},{:.2},{:.2}",
                    f.date,
                    f.symbol,
                    side(f.side),
                    f.shares,
                    f.price,
                    f.value,
                    f.cost
                )?;
            }
        }
    }
    files.append(
        "trades.csv",
        "date,symbol,side,shares,price,value,cost",
        &trades,
    )?;
    files.append(
        "equity.csv",
        &format!("date,cash,equity,{}", BENCHMARK.to_lowercase()),
        &equity,
    )?;
    files.append("events.log", "", &events)?;
    Ok(())
}

fn next_rebalance(a: &Account) -> String {
    match (a.config.schedule, a.last_rebalance) {
        (_, None) => "on the next close".into(),
        (Schedule::MonthStart, Some(last)) => {
            let (y, m) = if last.month() == 12 {
                (last.year() + 1, 1)
            } else {
                (last.year(), last.month() + 1)
            };
            let first = NaiveDate::from_ymd_opt(y, m, 1).map(|d| d.format("%B %Y").to_string());
            format!(
                "on the close of the first trading day of {}, trades at the next open",
                first.unwrap_or_default()
            )
        }
        (Schedule::EveryDays(n), Some(last)) => {
            format!("{n} trading days after {last}")
        }
    }
}

fn report(
    name: &str,
    a: &Account,
    p: &Panel,
    ranker: &dyn rotation::Ranker,
    progress: &Progress,
    bench: &HashMap<NaiveDate, f64>,
    equity_before: Option<f64>,
) -> String {
    let mut out = String::new();
    let mut alerts: Vec<String> = Vec::new();
    let Some(d) = p.dates().len().checked_sub(1) else {
        return out;
    };
    let day = p.date(d).unwrap_or_default();
    let traded = (0..p.stocks()).filter(|&s| p.get(s, d).is_some()).count();
    let traded_before = d
        .checked_sub(1)
        .map(|y| (0..p.stocks()).filter(|&s| p.get(s, y).is_some()).count())
        .unwrap_or(traded);
    let age = (Utc::now().date_naive() - day).num_days();
    if age > STALE_DAYS {
        alerts.push(format!(
            "Data ends {day}, {age} days ago: run scripts/rotation_daily.sh (or dhan-daily) to update"
        ));
    }
    if traded * 10 < traded_before * 8 {
        alerts.push(format!(
            "Only {traded} stocks have a bar on {day} against {traded_before} the day before: the download may be incomplete"
        ));
    }

    let equity = a.equity();
    let _ = writeln!(
        out,
        "Paper account '{name}': {}, top {}, exit rank {}, since {} with {}",
        ranker.name(),
        a.config.hold,
        a.config.exit_rank.unwrap_or(a.config.hold),
        a.start,
        lakh(a.start_capital)
    );
    let _ = writeln!(
        out,
        "Data through {day} ({traded} stocks traded). Places no orders.\n"
    );
    let base = bench
        .iter()
        .filter(|(date, _)| **date <= a.start)
        .max_by_key(|(date, _)| **date)
        .map(|(_, c)| *c);
    let nifty = match (base, bench.get(&day)) {
        (Some(b), Some(c)) if b > 0.0 => format!(", {BENCHMARK} {}", pct(c / b - 1.0)),
        _ => String::new(),
    };
    // Change over the last day processed: from the day before's close.
    let prev = match progress.days.len() {
        0 => None,
        1 => equity_before,
        n => progress.days.get(n - 2).map(|x| x.equity),
    };
    let today = prev
        .filter(|x| *x > 0.0)
        .map(|x| format!("   last day {}", pct(equity / x - 1.0)))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "Equity {}  ({} since start{nifty}){today}   drawdown {} from peak {}",
        lakh(equity),
        pct(equity / a.start_capital - 1.0),
        pct(-a.drawdown()),
        lakh(a.peak_equity)
    );
    let _ = writeln!(out, "Cash {}", lakh(a.cash));
    if !progress.days.is_empty() {
        let fills = progress
            .days
            .iter()
            .flat_map(|x| &x.events)
            .filter(|e| matches!(e, Event::Filled(_)))
            .count();
        let _ = writeln!(
            out,
            "This run: {} new trading day{} ({} to {}), {fills} fills",
            progress.days.len(),
            if progress.days.len() == 1 { "" } else { "s" },
            progress.days.first().map(|x| x.date).unwrap_or_default(),
            progress.days.last().map(|x| x.date).unwrap_or_default()
        );
    } else {
        let _ = writeln!(out, "No new trading days since the last run.");
    }

    // Ranks today (no capital filter), for the holdings table.
    let ranks: HashMap<&str, usize> = ranking(p, ranker, &a.config, d)
        .iter()
        .enumerate()
        .filter_map(|(i, (s, _))| Some((p.symbol(*s)?, i + 1)))
        .collect();
    let exit = a.config.exit_rank.unwrap_or(a.config.hold);
    let index: HashMap<&str, usize> = p
        .symbols()
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    let mut held = a.positions.clone();
    held.sort_by_key(|x| ranks.get(x.symbol.as_str()).copied().unwrap_or(usize::MAX));
    let _ = writeln!(
        out,
        "\nHoldings ({}), ranked today; a stock below rank {exit} at a rebalance is sold:",
        held.len()
    );
    let _ = writeln!(
        out,
        "  {:<14} {:>7} {:>10} {:>9} {:>9} {:>11} {:>7} {:>7} {:>5}  note",
        "symbol", "shares", "entry day", "entry", "last", "value", "P&L", "today", "rank"
    );
    for pos in &held {
        let s = index.get(pos.symbol.as_str()).copied();
        let today_ret = s.and_then(|s| p.ret(s, d.checked_sub(1)?, d));
        let rank = ranks.get(pos.symbol.as_str()).copied();
        let mut notes = Vec::new();
        match rank {
            None => {
                notes.push("out of universe".to_owned());
                alerts.push(format!(
                    "{} is no longer in the top-500 universe (or cannot be scored): it will be sold at the next rebalance",
                    pos.symbol
                ));
            }
            Some(r) if r > exit => {
                notes.push("below exit rank".to_owned());
                alerts.push(format!(
                    "{} ranks {r}, below {exit}: sold at the next rebalance unless it recovers",
                    pos.symbol
                ));
            }
            Some(r) if r * 4 > exit * 3 => notes.push("near exit".to_owned()),
            Some(_) => {}
        }
        if pos.last_date < day {
            notes.push(format!("no trade since {}", pos.last_date));
            alerts.push(format!(
                "{} has not traded since {}: check for a suspension, delisting or merger",
                pos.symbol, pos.last_date
            ));
        }
        if let Some(r) = today_ret.filter(|r| *r <= BIG_FALL) {
            alerts.push(format!(
                "{} fell {} today: check the news; no automatic sale (stops tested worse)",
                pos.symbol,
                pct(r)
            ));
        }
        let _ = writeln!(
            out,
            "  {:<14} {:>7} {:>10} {:>9.2} {:>9.2} {:>11} {:>7} {:>7} {:>5}  {}",
            pos.symbol,
            pos.shares,
            pos.entry_date,
            pos.entry_price,
            pos.last_close,
            lakh(pos.shares as f64 * pos.last_close),
            pct(pos.last_close / pos.entry_price - 1.0),
            today_ret.map(pct).unwrap_or_else(|| "-".into()),
            rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            notes.join(", ")
        );
    }

    match &a.pending {
        Some(pending) => {
            let _ = writeln!(
                out,
                "\nORDERS FOR THE NEXT OPEN (rebalance decided on the close of {}):",
                pending.decided
            );
            let per = equity / a.config.hold as f64;
            for pos in a
                .positions
                .iter()
                .filter(|x| !pending.targets.contains(&x.symbol))
            {
                let _ = writeln!(out, "  SELL {:>7} {}", pos.shares, pos.symbol);
            }
            for t in pending
                .targets
                .iter()
                .filter(|t| !a.positions.iter().any(|x| &x.symbol == *t))
            {
                let close = index
                    .get(t.as_str())
                    .and_then(|&s| p.get(s, d))
                    .map(|x| x.close);
                match close {
                    Some(c) => {
                        let _ = writeln!(
                            out,
                            "  BUY  {:>7} {t}  (about {} at today's close {c:.2})",
                            (per / c).floor(),
                            lakh(per)
                        );
                    }
                    None => {
                        let _ = writeln!(out, "  BUY  {t}  (about {})", lakh(per));
                    }
                }
            }
            let _ = writeln!(
                out,
                "  Kept: {} stocks. Buys use an equal share of equity at the open; sells go first.",
                a.positions
                    .iter()
                    .filter(|x| pending.targets.contains(&x.symbol))
                    .count()
            );
        }
        None => {
            let _ = writeln!(out, "\nNo orders pending.");
        }
    }
    let _ = writeln!(out, "Next rebalance: {}", next_rebalance(a));

    for e in progress
        .adjustments
        .iter()
        .chain(progress.days.iter().flat_map(|x| &x.events))
    {
        if matches!(
            e,
            Event::Adjusted { .. } | Event::NotFilled { .. } | Event::GaveUp { .. }
        ) {
            alerts.push(describe(e));
        }
    }
    let _ = writeln!(out, "\nAlerts:");
    if alerts.is_empty() {
        let _ = writeln!(out, "  none");
    }
    alerts.dedup();
    for x in &alerts {
        let _ = writeln!(out, "  - {x}");
    }
    out
}

async fn main_inner(args: Args) -> Result<(), Error> {
    let files = Files::new(&args.name)?;
    match &args.command {
        Command::Init { .. } => init(&args, &files, &args.command).await,
        Command::Run => run(&args, &files).await,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match main_inner(Args::parse()).await {
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
    fn next_rebalance_names_the_next_month() {
        let mut a = Account::new(
            "momentum_low_vol",
            serde_json::Value::Null,
            RotationConfig::recommended(),
            1e6,
            NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
        )
        .unwrap();
        assert_eq!(next_rebalance(&a), "on the next close");
        a.last_rebalance = NaiveDate::from_ymd_opt(2026, 12, 1);
        assert!(next_rebalance(&a).contains("January 2027"));
    }

    #[test]
    fn account_names_are_checked() {
        assert!(Files::new("default").is_ok());
        assert!(Files::new("../x").is_err());
        assert!(Files::new("").is_err());
    }
}
