//! Ledger and historical trade statements, including the actual charges
//! (STT, brokerage, exchange fees) paid per trade.

use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use serde::Deserialize;

use super::DhanClient;
use super::orders::Trade;
use crate::rate_limit::ApiCategory;
use crate::serde_util as su;
use crate::{DhanError, Result};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LedgerEntry {
    #[serde(
        default,
        rename = "dhanClientId",
        deserialize_with = "su::string_lenient"
    )]
    pub dhan_client_id: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub narration: String,
    /// e.g. `"Jun 22, 2022"`.
    #[serde(
        default,
        rename = "voucherdate",
        deserialize_with = "su::string_lenient"
    )]
    pub voucher_date: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub exchange: String,
    #[serde(
        default,
        rename = "voucherdesc",
        deserialize_with = "su::string_lenient"
    )]
    pub voucher_desc: String,
    #[serde(
        default,
        rename = "vouchernumber",
        deserialize_with = "su::string_lenient"
    )]
    pub voucher_number: String,
    #[serde(default, deserialize_with = "su::decimal")]
    pub debit: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub credit: Decimal,
    #[serde(default, rename = "runbal", deserialize_with = "su::decimal")]
    pub running_balance: Decimal,
}

/// A past trade with the charges levied on it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalTrade {
    #[serde(flatten)]
    pub trade: Trade,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub custom_symbol: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub isin: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub instrument: Option<String>,
    #[serde(default, deserialize_with = "su::decimal")]
    pub sebi_tax: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub stt: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub brokerage_charges: Decimal,
    /// GST.
    #[serde(default, deserialize_with = "su::decimal")]
    pub service_tax: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub exchange_transaction_charges: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub stamp_duty: Decimal,
}

impl HistoricalTrade {
    pub fn total_charges(&self) -> Decimal {
        self.sebi_tax
            + self.stt
            + self.brokerage_charges
            + self.service_tax
            + self.exchange_transaction_charges
            + self.stamp_duty
    }

    pub fn exchange_time(&self) -> Option<NaiveDateTime> {
        self.trade.exchange_time
    }
}

impl DhanClient {
    pub async fn ledger(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<LedgerEntry>> {
        let query = [
            ("from-date", from.format("%Y-%m-%d").to_string()),
            ("to-date", to.format("%Y-%m-%d").to_string()),
        ];
        let v: serde_json::Value = self
            .get_query("/ledger", &query, ApiCategory::NonTrading)
            .await?;
        su::one_or_many(v).map_err(|e| DhanError::decode("/ledger", e, b""))
    }

    /// One page (0-based) of trade history between two dates.
    pub async fn trade_history(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        page: u32,
    ) -> Result<Vec<HistoricalTrade>> {
        let path = format!(
            "/trades/{}/{}/{page}",
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );
        let v = self.get_value(&path, ApiCategory::NonTrading).await?;
        su::one_or_many(v).map_err(|e| DhanError::decode(&path, e, b""))
    }

    /// All pages of trade history between two dates.
    pub async fn trade_history_all(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<HistoricalTrade>> {
        const MAX_PAGES: u32 = 1000;
        let mut all = Vec::new();
        for page in 0..MAX_PAGES {
            let batch = self.trade_history(from, to, page).await?;
            if batch.is_empty() {
                return Ok(all);
            }
            all.extend(batch);
        }
        Err(DhanError::decode(
            "/trades/{from}/{to}/{page}",
            format!("more than {MAX_PAGES} pages; narrow the date range"),
            b"",
        ))
    }
}
