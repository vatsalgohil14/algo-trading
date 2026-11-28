//! Helpers shared by the tools.

/// Daily bars from QuestDB `bars_daily`.
pub mod daily {
    use chrono::NaiveDate;
    use rotation::Panel;

    pub type Error = Box<dyn std::error::Error>;

    /// Runs `sql` through QuestDB's CSV export.
    async fn export(qdb: &str, sql: &str) -> Result<String, Error> {
        Ok(reqwest::Client::new()
            .get(format!("http://{qdb}/exp"))
            .query(&[("query", sql)])
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }

    /// Company shares (source `dhan_daily`, no ETFs) from `from` to `to`
    /// (inclusive; `None` = all), as a [`Panel`].
    pub async fn stocks(qdb: &str, from: NaiveDate, to: Option<NaiveDate>) -> Result<Panel, Error> {
        Ok(Panel::from_csv(&stocks_csv(qdb, from, to).await?)?)
    }

    /// The same as CSV (`date,symbol,open,high,low,close,value`), to cache.
    pub async fn stocks_csv(
        qdb: &str,
        from: NaiveDate,
        to: Option<NaiveDate>,
    ) -> Result<String, Error> {
        // ts is midnight IST (18:30 UTC the day before): adding 5h30 gives
        // the trading date, and `< to + 1 day` includes `to`.
        let to = to
            .map(|t| format!(" and ts < '{}'", t + chrono::TimeDelta::days(1)))
            .unwrap_or_default();
        let sql = format!(
            "select to_str(dateadd('m', 330, ts), 'yyyy-MM-dd') d, symbol, open, high, low, close, \
             close * volume v from bars_daily where source = 'dhan_daily' and ts >= '{from}'{to} order by ts"
        );
        export(qdb, &sql).await
    }

    /// Daily closes of one ETF or fund (source `dhan_daily_fund`), e.g.
    /// `NIFTYBEES` as a market benchmark.
    pub async fn fund_closes(
        qdb: &str,
        symbol: &str,
        from: NaiveDate,
    ) -> Result<Vec<(NaiveDate, f64)>, Error> {
        if !symbol.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("bad symbol {symbol:?}").into());
        }
        let sql = format!(
            "select to_str(dateadd('m', 330, ts), 'yyyy-MM-dd') d, close from bars_daily \
             where source = 'dhan_daily_fund' and symbol = '{symbol}' and ts >= '{from}' order by ts"
        );
        let body = export(qdb, &sql).await?;
        let mut out = Vec::new();
        for line in body.lines().skip(1) {
            let f: Vec<&str> = line.split(',').map(|x| x.trim_matches('"')).collect();
            if let [d, c] = f.as_slice()
                && let (Ok(d), Ok(c)) = (d.parse::<NaiveDate>(), c.parse::<f64>())
            {
                out.push((d, c));
            }
        }
        Ok(out)
    }
}
