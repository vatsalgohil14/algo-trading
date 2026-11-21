//! Daily bars of many stocks aligned on one calendar.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;

use crate::RotationError;

/// A one-day move beyond this (up or down, relative) is a data break: an
/// unadjusted corporate action or a relisting after a long suspension. The
/// stock's history restarts after it, so no signal or return spans it.
pub const BREAK: f64 = 0.5;
/// More missing trading days in a row than this also restarts the history.
pub const MAX_GAP_DAYS: usize = 10;
/// Trading days in a year, for annualising.
pub const TRADING_DAYS: f64 = 248.0;
/// Trading days in a month.
pub const MONTH: usize = 21;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Day {
    pub open: f64,
    pub high: f64,
    /// The day's low; `min(open, close)` when the source had no low.
    pub low: f64,
    pub close: f64,
    /// Traded value in rupees: close × volume.
    pub value: f64,
}

/// `days[stock][date]`, with a stretch id per cell: two cells in the same
/// stretch are joined by unbroken history.
#[derive(Debug, Clone)]
pub struct Panel {
    symbols: Vec<String>,
    dates: Vec<NaiveDate>,
    days: Vec<Vec<Option<Day>>>,
    stretch: Vec<Vec<u32>>,
}

impl Panel {
    /// From CSV with a header and columns `date,symbol,open,high,close,value`
    /// or `date,symbol,open,high,low,close,value` (as the tools export from
    /// `bars_daily`). Quotes around fields are allowed. Rows with impossible
    /// prices are skipped.
    pub fn from_csv(text: &str) -> Result<Self, RotationError> {
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut symbols: Vec<String> = Vec::new();
        let mut rows: BTreeMap<NaiveDate, Vec<(usize, Day)>> = BTreeMap::new();
        for (n, line) in text.lines().enumerate().skip(1) {
            let f: Vec<&str> = line
                .split(',')
                .map(|x| x.trim().trim_matches('"'))
                .collect();
            let (date, symbol, open, high, low, close, value) = match f.as_slice() {
                [d, s, o, h, c, v] => (*d, *s, *o, *h, None, *c, *v),
                [d, s, o, h, l, c, v] => (*d, *s, *o, *h, Some(*l), *c, *v),
                _ => continue,
            };
            let bad = |what: &str| RotationError::Data(format!("line {}: bad {what}", n + 1));
            let date: NaiveDate = date.parse().map_err(|_| bad("date"))?;
            let num = |x: &str, what: &str| x.parse::<f64>().map_err(|_| bad(what));
            let (open, close) = (num(open, "open")?, num(close, "close")?);
            let day = Day {
                open,
                high: num(high, "high")?,
                low: match low {
                    Some(l) => num(l, "low")?,
                    None => open.min(close),
                },
                close,
                value: num(value, "value")?,
            };
            let sane = day.low > 0.0
                && day.low <= day.open.min(day.close)
                && day.high >= day.open.max(day.close);
            if !sane {
                continue;
            }
            let s = match index.get(symbol) {
                Some(&s) => s,
                None => {
                    symbols.push(symbol.to_owned());
                    index.insert(symbol.to_owned(), symbols.len() - 1);
                    symbols.len() - 1
                }
            };
            rows.entry(date).or_default().push((s, day));
        }
        Ok(Self::build(symbols, rows))
    }

    fn build(symbols: Vec<String>, rows: BTreeMap<NaiveDate, Vec<(usize, Day)>>) -> Self {
        let dates: Vec<NaiveDate> = rows.keys().copied().collect();
        let mut days = vec![vec![None; dates.len()]; symbols.len()];
        for (d, row) in rows.values().enumerate() {
            for &(s, day) in row {
                if let Some(cell) = days.get_mut(s).and_then(|r| r.get_mut(d)) {
                    *cell = Some(day);
                }
            }
        }
        let stretch = days
            .iter()
            .map(|row| {
                let (mut id, mut prev, mut gap) = (0u32, None::<Day>, 0usize);
                row.iter()
                    .map(|cell| {
                        match cell {
                            Some(t) => {
                                let jump = prev.is_none_or(|p| {
                                    let r = t.open / p.close;
                                    r > 1.0 + BREAK || r < 1.0 / (1.0 + BREAK)
                                });
                                if jump || gap > MAX_GAP_DAYS {
                                    id += 1;
                                }
                                prev = Some(*t);
                                gap = 0;
                            }
                            None => gap += 1,
                        }
                        id
                    })
                    .collect()
            })
            .collect();
        Self {
            symbols,
            dates,
            days,
            stretch,
        }
    }

    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    pub fn dates(&self) -> &[NaiveDate] {
        &self.dates
    }

    pub fn stocks(&self) -> usize {
        self.symbols.len()
    }

    pub fn symbol(&self, s: usize) -> Option<&str> {
        self.symbols.get(s).map(String::as_str)
    }

    pub fn date(&self, d: usize) -> Option<NaiveDate> {
        self.dates.get(d).copied()
    }

    /// Index of the last trading day on or before `date`.
    pub fn day_on_or_before(&self, date: NaiveDate) -> Option<usize> {
        self.dates.iter().rposition(|d| *d <= date)
    }

    /// Index of the first trading day on or after `date`.
    pub fn day_on_or_after(&self, date: NaiveDate) -> Option<usize> {
        self.dates.iter().position(|d| *d >= date)
    }

    pub fn get(&self, s: usize, d: usize) -> Option<Day> {
        *self.days.get(s)?.get(d)?
    }

    fn stretch(&self, s: usize, d: usize) -> Option<u32> {
        self.stretch.get(s)?.get(d).copied()
    }

    /// Both days present and joined by unbroken history.
    pub fn joined(&self, s: usize, a: usize, b: usize) -> bool {
        self.get(s, a).is_some()
            && self.get(s, b).is_some()
            && self
                .stretch(s, a)
                .is_some_and(|x| Some(x) == self.stretch(s, b))
    }

    /// Close-to-close return from day `a` to day `b`.
    pub fn ret(&self, s: usize, a: usize, b: usize) -> Option<f64> {
        (a <= b && self.joined(s, a, b)).then_some(())?;
        Some(self.get(s, b)?.close / self.get(s, a)?.close - 1.0)
    }

    /// Momentum on day `d`: the return from `lookback` to `skip` trading days
    /// ago. Skipping the last month avoids its short-term reversal.
    pub fn momentum(&self, s: usize, d: usize, lookback: usize, skip: usize) -> Option<f64> {
        self.ret(s, d.checked_sub(lookback)?, d.checked_sub(skip)?)
    }

    /// Daily close-to-close returns over the `k` days ending on `d`; `None`
    /// unless every day is present.
    pub fn daily_returns(&self, s: usize, d: usize, k: usize) -> Option<Vec<f64>> {
        let a = d.checked_sub(k)?;
        self.joined(s, a, d).then_some(())?;
        (a + 1..=d)
            .map(|i| Some(self.get(s, i)?.close / self.get(s, i.checked_sub(1)?)?.close - 1.0))
            .collect()
    }

    /// Annualised volatility of daily returns over `k` days ending on `d`.
    pub fn volatility(&self, s: usize, d: usize, k: usize) -> Option<f64> {
        let r = self.daily_returns(s, d, k)?;
        let n = r.len();
        (n >= 2).then_some(())?;
        let m = r.iter().sum::<f64>() / n as f64;
        let var = r.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1) as f64;
        Some(var.sqrt() * TRADING_DAYS.sqrt())
    }

    /// Median daily traded value over the `k` days before `d`.
    pub fn median_value(&self, s: usize, d: usize, k: usize) -> Option<f64> {
        let a = d.checked_sub(k)?;
        let mut v: Vec<f64> = (a..d)
            .filter_map(|i| self.get(s, i))
            .map(|x| x.value)
            .collect();
        (v.len() * 5 >= k * 4).then_some(())?;
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied()
    }

    /// The return of buying at the open of `entry` and selling at the open
    /// of `exit`. If the stock's history ends or breaks first, it is sold at
    /// its last close before that.
    pub fn hold_return(&self, s: usize, entry: usize, exit: usize) -> Option<f64> {
        self.hold_with_stops(s, entry, exit, None, None)
            .map(|x| x.0)
    }

    /// Like [`hold_return`](Self::hold_return), with stops checked on each
    /// close before `exit`: a close at or below `stop` × entry price (fixed
    /// stop, e.g. 0.85) or `trailing` × the highest close since entry
    /// (trailing stop) sells at the next open. Returns the return and
    /// whether a stop fired; after a stop the money waits in cash.
    pub fn hold_with_stops(
        &self,
        s: usize,
        entry: usize,
        exit: usize,
        stop: Option<f64>,
        trailing: Option<f64>,
    ) -> Option<(f64, bool)> {
        let buy = self.get(s, entry)?.open;
        let id = self.stretch(s, entry)?;
        let mut last = None;
        let mut peak = buy;
        let mut stop_hit = false;
        for d in entry..=exit {
            if self.stretch(s, d) != Some(id) {
                break;
            }
            let Some(x) = self.get(s, d) else {
                continue;
            };
            if d == exit || stop_hit {
                // Sold at this open: the planned exit, or the day after a stop.
                last = Some(x.open);
                break;
            }
            last = Some(x.close);
            peak = peak.max(x.close);
            let fixed = stop.is_some_and(|f| x.close <= buy * f);
            let trail = trailing.is_some_and(|f| x.close <= peak * f);
            stop_hit = fixed || trail;
        }
        Some((last? / buy - 1.0, stop_hit))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// One row per stock, one column per day; `None` = no trading.
    pub(crate) fn panel(closes: &[&[Option<f64>]]) -> Panel {
        let d0 = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
        let mut text = String::from("date,symbol,open,high,close,value\n");
        for (s, row) in closes.iter().enumerate() {
            for (i, c) in row.iter().enumerate() {
                if let Some(c) = c {
                    let d = d0 + chrono::TimeDelta::days(i as i64);
                    text.push_str(&format!(
                        "{d},S{s},{c},{c},{c},{}\n",
                        1_000_000.0 + s as f64
                    ));
                }
            }
        }
        Panel::from_csv(&text).unwrap()
    }

    #[test]
    fn parses_quoted_csv() {
        let p = Panel::from_csv(
            "\"d\",\"symbol\",\"open\",\"high\",\"close\",\"v\"\n\"2025-01-02\",\"A\",10,11,10.5,100\n",
        )
        .unwrap();
        assert_eq!(p.symbols(), ["A"]);
        assert_eq!(p.get(0, 0).unwrap().close, 10.5);
        assert_eq!(p.get(0, 0).unwrap().low, 10.0); // no low column: min(open, close)
        let p = Panel::from_csv(
            "d,s,o,h,l,c,v\n2025-01-02,A,10,11,9,10.5,100\n2025-01-03,A,10,11,10.6,10.5,1\n",
        )
        .unwrap();
        assert_eq!(p.get(0, 0).unwrap().low, 9.0);
        assert!(p.get(0, 1).is_none()); // low above the close: skipped
    }

    #[test]
    fn breaks_and_gaps_split_history() {
        let p = panel(&[&[Some(100.0), Some(110.0), Some(30.0), Some(33.0)]]);
        assert!(p.joined(0, 0, 1));
        assert!(!p.joined(0, 1, 2)); // -73%: a break
        assert!((p.ret(0, 2, 3).unwrap() - 0.1).abs() < 1e-9);
        assert!(p.ret(0, 0, 3).is_none());
        let mut row = vec![Some(100.0)];
        row.extend(std::iter::repeat_n(None, MAX_GAP_DAYS + 1));
        row.push(Some(100.0));
        let p = panel(&[&row]);
        assert!(!p.joined(0, 0, row.len() - 1));
    }

    #[test]
    fn hold_return_sells_at_last_close_when_data_ends() {
        let p = panel(&[&[Some(100.0), Some(100.0), Some(120.0), None, None]]);
        assert!((p.hold_return(0, 1, 4).unwrap() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn stops_sell_at_the_next_open() {
        // Bought at 100; closes 95, 84 (below an 85% stop), then opens at 83.
        let p = panel(&[&[
            Some(100.0),
            Some(95.0),
            Some(84.0),
            Some(83.0),
            Some(90.0),
            Some(120.0),
        ]]);
        let (r, hit) = p.hold_with_stops(0, 0, 5, Some(0.85), None).unwrap();
        assert!(hit);
        assert!((r - (83.0 / 100.0 - 1.0)).abs() < 1e-9);
        // Without a stop it rides to the exit open.
        let (r, hit) = p.hold_with_stops(0, 0, 5, None, None).unwrap();
        assert!(!hit && (r - 0.2).abs() < 1e-9);
        // Trailing 80% of the peak: peak 130, close 100 is below 104.
        let p = panel(&[&[
            Some(100.0),
            Some(130.0),
            Some(100.0),
            Some(101.0),
            Some(140.0),
        ]]);
        let (r, hit) = p.hold_with_stops(0, 0, 4, None, Some(0.8)).unwrap();
        assert!(hit && (r - 0.01).abs() < 1e-9);
    }

    #[test]
    fn momentum_and_volatility() {
        let row: Vec<Option<f64>> = (0..300).map(|i| Some(100.0 * 1.001f64.powi(i))).collect();
        let p = panel(&[&row]);
        let m = p.momentum(0, 299, 252, 21).unwrap();
        assert!((m - (1.001f64.powi(231) - 1.0)).abs() < 1e-9);
        assert!(p.volatility(0, 299, 252).unwrap() < 1e-9); // constant growth
    }
}
