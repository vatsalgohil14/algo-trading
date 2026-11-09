//! Turns a run into numbers, and compares them with the pass mark.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::io::Write as _;

use chrono::Datelike;
use engine::ClosedTrade;
use trading_core::Money;

use crate::{BacktestError, BacktestResult, DayResult};

/// Results of a group of trades.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    pub trades: u64,
    pub wins: u64,
    pub gross: Money,
    pub charges: Money,
    pub net: Money,
    pub won: Money,
    pub lost: Money,
    r_total: f64,
    r_count: u64,
}

impl Group {
    fn add(&mut self, t: &ClosedTrade) {
        self.trades += 1;
        self.gross += t.gross;
        self.charges += t.charges;
        self.net += t.net;
        if t.net.paise() > 0 {
            self.wins += 1;
            self.won += t.net;
        } else {
            self.lost += t.net.abs();
        }
        if let Some(r) = t.r_multiple() {
            self.r_total += r;
            self.r_count += 1;
        }
    }

    pub fn win_rate(&self) -> f64 {
        ratio(self.wins as f64, self.trades as f64)
    }

    pub fn average_r(&self) -> f64 {
        ratio(self.r_total, self.r_count as f64)
    }

    /// Money won per rupee lost, after charges.
    pub fn profit_factor(&self) -> f64 {
        if self.lost.paise() == 0 {
            if self.won.paise() > 0 {
                f64::INFINITY
            } else {
                0.0
            }
        } else {
            self.won.as_f64() / self.lost.as_f64()
        }
    }
}

fn ratio(a: f64, b: f64) -> f64 {
    if b == 0.0 { 0.0 } else { a / b }
}

/// One line of the pass-mark check.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub needed: String,
    pub actual: String,
    pub passed: bool,
}

/// The headline numbers of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub overall: Group,
    pub by_setup: BTreeMap<&'static str, Group>,
    pub by_exit: BTreeMap<&'static str, Group>,
    pub by_month: BTreeMap<(i32, u32), Money>,
    pub days: usize,
    pub max_drawdown: Money,
    pub return_pct: f64,
    pub annual_return_pct: f64,
    pub profitable_months_pct: f64,
    pub checks: Vec<Check>,
}

/// Trading days in a year, for annualising.
const DAYS_PER_YEAR: f64 = 250.0;

impl Summary {
    pub fn of(result: &BacktestResult) -> Self {
        let mut overall = Group::default();
        let mut by_setup: BTreeMap<&'static str, Group> = BTreeMap::new();
        let mut by_exit: BTreeMap<&'static str, Group> = BTreeMap::new();
        for t in &result.trades {
            overall.add(t);
            by_setup.entry(t.setup).or_default().add(t);
            by_exit.entry(t.exit_reason).or_default().add(t);
        }

        let mut by_month: BTreeMap<(i32, u32), Money> = BTreeMap::new();
        for DayResult { date, pnl } in &result.days {
            *by_month.entry((date.year(), date.month())).or_default() += *pnl;
        }
        let (mut equity, mut peak, mut max_drawdown) = (Money::ZERO, Money::ZERO, Money::ZERO);
        for day in &result.days {
            equity += day.pnl;
            peak = peak.max(equity);
            max_drawdown = max_drawdown.max(peak - equity);
        }

        let capital = result.capital.as_f64();
        let return_pct = ratio(equity.as_f64(), capital) * 100.0;
        let years = result.days.len() as f64 / DAYS_PER_YEAR;
        let annual_return_pct = ratio(return_pct, years);
        let up = by_month.values().filter(|m| m.paise() > 0).count() as f64;
        let profitable_months_pct = ratio(up, by_month.len() as f64) * 100.0;
        let drawdown_pct = ratio(max_drawdown.as_f64(), capital) * 100.0;

        let check = |name, needed: &str, actual: String, passed| Check {
            name,
            needed: needed.to_owned(),
            actual,
            passed,
        };
        let checks = vec![
            check(
                "Trades",
                ">= 300",
                overall.trades.to_string(),
                overall.trades >= 300,
            ),
            check(
                "Profit factor after costs",
                ">= 1.30",
                format!("{:.2}", overall.profit_factor()),
                overall.profit_factor() >= 1.3,
            ),
            check(
                "Average per trade",
                ">= 0.15R",
                format!("{:.3}R", overall.average_r()),
                overall.average_r() >= 0.15,
            ),
            check(
                "Maximum drawdown",
                "<= 10% of capital",
                format!("{drawdown_pct:.1}%"),
                drawdown_pct <= 10.0,
            ),
            check(
                "Profitable months",
                ">= 60%",
                format!("{profitable_months_pct:.0}%"),
                profitable_months_pct >= 60.0,
            ),
        ];

        Self {
            overall,
            by_setup,
            by_exit,
            by_month,
            days: result.days.len(),
            max_drawdown,
            return_pct,
            annual_return_pct,
            profitable_months_pct,
            checks,
        }
    }

    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// The report as text.
    pub fn render(&self, result: &BacktestResult) -> Result<String, fmt::Error> {
        let mut s = String::new();
        let o = &self.overall;
        writeln!(s, "RESULT")?;
        writeln!(s, "  Days                {}", self.days)?;
        writeln!(s, "  Trades              {}", o.trades)?;
        writeln!(s, "  Win rate            {:.1}%", o.win_rate() * 100.0)?;
        writeln!(s, "  Gross P&L           ₹{}", o.gross)?;
        writeln!(s, "  Charges             ₹{}", o.charges)?;
        writeln!(s, "  Net P&L             ₹{}", o.net)?;
        writeln!(s, "  Return on capital   {:.1}%", self.return_pct)?;
        writeln!(s, "  Annualised          {:.1}%", self.annual_return_pct)?;
        writeln!(s, "  Profit factor       {:.2}", o.profit_factor())?;
        writeln!(s, "  Average per trade   {:.3}R", o.average_r())?;
        writeln!(s, "  Maximum drawdown    ₹{}", self.max_drawdown)?;

        writeln!(s, "\nPASS MARK (a single run; validation needs more)")?;
        for c in &self.checks {
            let verdict = if c.passed { "pass" } else { "FAIL" };
            writeln!(
                s,
                "  {verdict}  {:<28} need {:<20} got {}",
                c.name, c.needed, c.actual
            )?;
        }

        write_groups(&mut s, "BY SETUP", &self.by_setup)?;
        write_groups(&mut s, "BY EXIT", &self.by_exit)?;

        writeln!(s, "\nBY MONTH")?;
        for ((year, month), pnl) in &self.by_month {
            writeln!(s, "  {year}-{month:02}  ₹{pnl}")?;
        }

        for (strategy, counts) in &result.filter_counts {
            writeln!(s, "\nFILTERS: {strategy} (stock-days blocked)")?;
            write_counts(&mut s, counts)?;
        }
        writeln!(s, "\nREFUSED BY RISK MANAGER")?;
        write_counts(&mut s, &result.risk_rejections)?;

        writeln!(s, "\nHEALTH (anything but zero deserves a look)")?;
        writeln!(s, "  Bad bars            {}", result.bad_bars)?;
        writeln!(s, "  Auto square-offs    {}", result.auto_square_offs)?;
        writeln!(s, "  Reconcile failures  {}", result.reconcile_failures)?;
        writeln!(s, "  Orders failed       {}", result.stats.orders_failed)?;
        writeln!(s, "  Stops not moved     {}", result.stats.stops_not_moved)?;
        writeln!(s, "  Daily loss halts    {}", result.stats.daily_loss_halts)?;
        Ok(s)
    }
}

fn write_groups(
    s: &mut String,
    title: &str,
    groups: &BTreeMap<&'static str, Group>,
) -> fmt::Result {
    writeln!(s, "\n{title}")?;
    for (name, g) in groups {
        writeln!(
            s,
            "  {name:<22} {:>6} trades  win {:>5.1}%  avg {:>7.3}R  net ₹{}",
            g.trades,
            g.win_rate() * 100.0,
            g.average_r(),
            g.net
        )?;
    }
    Ok(())
}

fn write_counts(s: &mut String, counts: &[(&'static str, u64)]) -> fmt::Result {
    for (name, n) in counts.iter().filter(|(_, n)| *n > 0) {
        writeln!(s, "  {name:<22} {n}")?;
    }
    Ok(())
}

fn file_error(path: &str, e: std::io::Error) -> BacktestError {
    BacktestError::File {
        path: path.to_owned(),
        why: e.to_string(),
    }
}

/// Writes one line per trade, for analysis elsewhere.
pub fn write_trades_csv(result: &BacktestResult, path: &str) -> Result<(), BacktestError> {
    let mut out =
        std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| file_error(path, e))?);
    let mut write = || -> std::io::Result<()> {
        writeln!(
            out,
            "symbol,setup,exit_reason,side,qty,entry,exit,opened,closed,gross,charges,net,r"
        )?;
        for t in &result.trades {
            let symbol = result
                .symbols
                .get(t.instrument.index())
                .map_or("?", String::as_str);
            let r = t.r_multiple().map_or(String::new(), |r| format!("{r:.3}"));
            writeln!(
                out,
                "{symbol},{},{},{},{},{},{},{},{},{},{},{},{r}",
                t.setup,
                t.exit_reason,
                t.side.as_str(),
                t.qty,
                t.entry_price,
                t.exit_price,
                t.opened,
                t.closed,
                t.gross,
                t.charges,
                t.net,
            )?;
        }
        out.flush()
    };
    write().map_err(|e| file_error(path, e))
}

/// Writes one line per day.
pub fn write_daily_csv(result: &BacktestResult, path: &str) -> Result<(), BacktestError> {
    let mut out =
        std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| file_error(path, e))?);
    let mut write = || -> std::io::Result<()> {
        writeln!(out, "date,pnl,cumulative")?;
        let mut total = Money::ZERO;
        for day in &result.days {
            total += day.pnl;
            writeln!(out, "{},{},{total}", day.date, day.pnl)?;
        }
        out.flush()
    };
    write().map_err(|e| file_error(path, e))
}
