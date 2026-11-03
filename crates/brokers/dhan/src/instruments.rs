//! Instrument master: maps symbols to Dhan security IDs and carries the
//! trading metadata needed before placing an order (tick size, lot size,
//! circuit limits, surveillance flags, MTF leverage).
//!
//! The detailed CSV is ~35 MB and changes daily; download it once each
//! morning and keep it in memory.

use std::collections::HashMap;
use std::io::Read;
use std::str::FromStr;

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Deserializer};

use crate::serde_util::decimal_from_value;
use crate::{DhanError, ExchangeSegment, InstrumentId, InstrumentKind, Result, SecurityId};

pub const DETAILED_CSV_URL: &str = "https://images.dhan.co/api-data/api-scrip-master-detailed.csv";

/// One row of the detailed instrument master.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Instrument {
    #[serde(rename = "EXCH_ID")]
    pub exchange: String,
    /// `E` equity, `D` derivatives, `C` currency, `M` commodity, `I` index.
    #[serde(rename = "SEGMENT")]
    pub segment_code: String,
    #[serde(rename = "SECURITY_ID")]
    pub security_id: SecurityId,
    #[serde(rename = "ISIN", deserialize_with = "na_string")]
    pub isin: Option<String>,
    #[serde(rename = "INSTRUMENT")]
    pub instrument: String,
    #[serde(rename = "UNDERLYING_SECURITY_ID", deserialize_with = "na_string")]
    pub underlying_security_id: Option<String>,
    /// For equities and indices this is the exchange ticker (`RELIANCE`);
    /// for derivatives, the underlying. See [`trading_symbol`](Self::trading_symbol).
    #[serde(rename = "UNDERLYING_SYMBOL", deserialize_with = "na_string")]
    pub underlying_symbol: Option<String>,
    /// Company name for equities (`RELIANCE INDUSTRIES LTD`), contract
    /// symbol for derivatives (`RELIANCE-Nov2026-FUT`).
    #[serde(rename = "SYMBOL_NAME")]
    pub symbol_name: String,
    #[serde(rename = "DISPLAY_NAME")]
    pub display_name: String,
    #[serde(rename = "INSTRUMENT_TYPE")]
    pub instrument_type: String,
    /// `EQ`, `BE`, `SM`, ... for NSE equities.
    #[serde(rename = "SERIES", deserialize_with = "na_string")]
    pub series: Option<String>,
    #[serde(rename = "LOT_SIZE", deserialize_with = "csv_decimal")]
    pub lot_size: Decimal,
    #[serde(rename = "SM_EXPIRY_DATE", deserialize_with = "na_string")]
    pub expiry_date: Option<String>,
    #[serde(rename = "STRIKE_PRICE", deserialize_with = "csv_decimal")]
    pub strike_price: Decimal,
    #[serde(rename = "OPTION_TYPE", deserialize_with = "na_string")]
    pub option_type: Option<String>,
    /// As published: paise for tradable segments, rupees for indices. Use
    /// [`tick_size`](Self::tick_size) instead.
    #[serde(rename = "TICK_SIZE", deserialize_with = "csv_decimal")]
    pub tick_size_raw: Decimal,
    #[serde(rename = "BRACKET_FLAG", deserialize_with = "flag")]
    pub bracket_allowed: bool,
    #[serde(rename = "COVER_FLAG", deserialize_with = "flag")]
    pub cover_allowed: bool,
    /// `Y` = under ASM/GSM surveillance; usually best avoided intraday.
    #[serde(rename = "ASM_GSM_FLAG", deserialize_with = "na_string")]
    pub asm_gsm_flag: Option<String>,
    #[serde(rename = "ASM_GSM_CATEGORY", deserialize_with = "na_string")]
    pub asm_gsm_category: Option<String>,
    /// `A` when both buying and selling are allowed.
    #[serde(rename = "BUY_SELL_INDICATOR", deserialize_with = "na_string")]
    pub buy_sell_indicator: Option<String>,
    #[serde(rename = "MTF_LEVERAGE", deserialize_with = "csv_decimal", default)]
    pub mtf_leverage: Decimal,
    #[serde(rename = "SM_UPPER_LIMIT", deserialize_with = "csv_decimal", default)]
    pub upper_circuit: Decimal,
    #[serde(rename = "SM_LOWER_LIMIT", deserialize_with = "csv_decimal", default)]
    pub lower_circuit: Decimal,
    #[serde(rename = "SM_FREEZE_QTY", deserialize_with = "csv_decimal", default)]
    pub freeze_qty: Decimal,
}

fn na_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let s = String::deserialize(d)?;
    let t = s.trim();
    Ok((!t.is_empty() && !t.eq_ignore_ascii_case("NA") && t != "XX").then(|| t.to_owned()))
}

fn csv_decimal<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
    let s = String::deserialize(d)?;
    decimal_from_value(&serde_json::Value::String(s))
        .map(Option::unwrap_or_default)
        .map_err(serde::de::Error::custom)
}

fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let s = String::deserialize(d)?;
    Ok(s.trim().eq_ignore_ascii_case("Y"))
}

impl Instrument {
    pub fn exchange_segment(&self) -> Option<ExchangeSegment> {
        Some(match (self.exchange.as_str(), self.segment_code.as_str()) {
            (_, "I") => ExchangeSegment::Index,
            ("NSE", "E") => ExchangeSegment::NseEq,
            ("NSE", "D") => ExchangeSegment::NseFno,
            ("NSE", "C") => ExchangeSegment::NseCurrency,
            ("BSE", "E") => ExchangeSegment::BseEq,
            ("BSE", "D") => ExchangeSegment::BseFno,
            ("BSE", "C") => ExchangeSegment::BseCurrency,
            ("MCX", "M") => ExchangeSegment::McxComm,
            _ => return None,
        })
    }

    pub fn id(&self) -> Option<InstrumentId> {
        self.exchange_segment().map(|segment| InstrumentId {
            segment,
            security_id: self.security_id,
        })
    }

    /// The symbol the exchange trades this under: `RELIANCE` for the stock,
    /// `RELIANCE-Nov2026-FUT` for a future.
    pub fn trading_symbol(&self) -> &str {
        match (
            self.segment_code.as_str(),
            self.underlying_symbol.as_deref(),
        ) {
            ("E" | "I", Some(sym)) => sym,
            _ => &self.symbol_name,
        }
    }

    pub fn kind(&self) -> Option<InstrumentKind> {
        InstrumentKind::from_str(&self.instrument).ok()
    }

    /// Minimum price step in rupees (e.g. ₹0.05).
    pub fn tick_size(&self) -> Decimal {
        if self.segment_code == "I" {
            self.tick_size_raw.normalize()
        } else {
            (self.tick_size_raw / Decimal::ONE_HUNDRED).normalize()
        }
    }

    /// Rounds a price to a valid tick. Buys round down and sells round up,
    /// so a limit price never becomes more aggressive than intended.
    pub fn round_to_tick(&self, price: Decimal, round_down: bool) -> Decimal {
        let tick = self.tick_size();
        if tick <= Decimal::ZERO {
            return price;
        }
        let strategy = if round_down {
            RoundingStrategy::ToNegativeInfinity
        } else {
            RoundingStrategy::ToPositiveInfinity
        };
        (price / tick).round_dp_with_strategy(0, strategy) * tick
    }

    pub fn is_under_surveillance(&self) -> bool {
        self.asm_gsm_flag.as_deref() == Some("Y")
    }
}

/// In-memory instrument master with lookups by symbol and by ID.
#[derive(Debug, Default)]
pub struct InstrumentMaster {
    instruments: Vec<Instrument>,
    by_id: HashMap<InstrumentId, usize>,
    by_symbol: HashMap<(ExchangeSegment, String), usize>,
}

impl InstrumentMaster {
    /// Parses the detailed CSV. Rows that fail to parse are skipped and
    /// counted; a file where most rows fail is rejected.
    pub fn from_reader<R: Read>(reader: R) -> Result<Self> {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(reader);
        let mut master = Self::default();
        let mut bad_rows = 0usize;
        for row in rdr.deserialize::<Instrument>() {
            match row {
                Ok(inst) => master.insert(inst),
                Err(e) => {
                    bad_rows += 1;
                    if bad_rows <= 5 {
                        tracing::warn!(error = %e, "skipping unparseable instrument row");
                    }
                }
            }
        }
        let parsed = master.instruments.len();
        if parsed == 0 || parsed < bad_rows {
            return Err(DhanError::Instruments(format!(
                "parsed {parsed} rows, {bad_rows} failed; the file format may have changed"
            )));
        }
        if bad_rows > 0 {
            tracing::warn!(
                bad_rows,
                parsed = master.instruments.len(),
                "instrument master loaded with skipped rows"
            );
        }
        Ok(master)
    }

    /// Downloads and parses the detailed instrument master.
    pub async fn download(http: &reqwest::Client) -> Result<Self> {
        let resp = http
            .get(DETAILED_CSV_URL)
            .send()
            .await
            .map_err(crate::DhanError::transport)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(DhanError::Instruments(format!(
                "download failed with HTTP {status}"
            )));
        }
        let bytes = resp.bytes().await.map_err(crate::DhanError::transport)?;
        // Parsing ~200k rows is CPU work; keep it off the async workers.
        tokio::task::spawn_blocking(move || Self::from_reader(bytes.as_ref()))
            .await
            .map_err(|e| DhanError::Instruments(format!("parser task failed: {e}")))?
    }

    fn insert(&mut self, inst: Instrument) {
        let idx = self.instruments.len();
        if let Some(id) = inst.id() {
            self.by_id.insert(id, idx);
            let key = (id.segment, inst.trading_symbol().to_ascii_uppercase());
            // Prefer the regular series (EQ on NSE) when a symbol has several.
            let replace = match self
                .by_symbol
                .get(&key)
                .and_then(|&i| self.instruments.get(i))
            {
                None => true,
                Some(existing) => {
                    existing.series.as_deref() != Some("EQ") && inst.series.as_deref() == Some("EQ")
                }
            };
            if replace {
                self.by_symbol.insert(key, idx);
            }
        }
        self.instruments.push(inst);
    }

    pub fn len(&self) -> usize {
        self.instruments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Instrument> {
        self.instruments.iter()
    }

    pub fn get(&self, id: InstrumentId) -> Option<&Instrument> {
        self.by_id.get(&id).and_then(|&i| self.instruments.get(i))
    }

    /// Looks up by exchange symbol, e.g. `(NseEq, "RELIANCE")`.
    pub fn find(&self, segment: ExchangeSegment, symbol: &str) -> Option<&Instrument> {
        self.by_symbol
            .get(&(segment, symbol.trim().to_ascii_uppercase()))
            .and_then(|&i| self.instruments.get(i))
    }

    /// Shortcut for NSE cash equities.
    pub fn nse_equity(&self, symbol: &str) -> Option<&Instrument> {
        self.find(ExchangeSegment::NseEq, symbol)
    }

    /// Resolves a list of NSE symbols, reporting all unknown ones at once.
    pub fn resolve_nse_equities<'a, I>(&self, symbols: I) -> Result<Vec<&Instrument>>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for s in symbols {
            match self.nse_equity(s) {
                Some(i) => found.push(i),
                None => missing.push(s.to_owned()),
            }
        }
        if missing.is_empty() {
            Ok(found)
        } else {
            Err(DhanError::Instruments(format!(
                "unknown NSE symbols: {}",
                missing.join(", ")
            )))
        }
    }
}
