//! Enums and identifiers shared across the Dhan APIs (see the Annexure in
//! Dhan's documentation).
//!
//! Enums that appear in responses have an `Unknown` variant so that a value
//! Dhan adds later degrades gracefully instead of failing the whole response.
//! Sending an `Unknown` value in a request is rejected locally.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::DhanError;

/// Exchange + segment pair. The numeric code is what the binary feed uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExchangeSegment {
    #[serde(rename = "IDX_I")]
    Index,
    #[serde(rename = "NSE_EQ")]
    NseEq,
    #[serde(rename = "NSE_FNO")]
    NseFno,
    #[serde(rename = "NSE_CURRENCY")]
    NseCurrency,
    #[serde(rename = "BSE_EQ")]
    BseEq,
    #[serde(rename = "MCX_COMM")]
    McxComm,
    #[serde(rename = "BSE_CURRENCY")]
    BseCurrency,
    #[serde(rename = "BSE_FNO")]
    BseFno,
}

impl ExchangeSegment {
    pub const ALL: [ExchangeSegment; 8] = [
        Self::Index,
        Self::NseEq,
        Self::NseFno,
        Self::NseCurrency,
        Self::BseEq,
        Self::McxComm,
        Self::BseCurrency,
        Self::BseFno,
    ];

    pub const fn code(self) -> u8 {
        match self {
            Self::Index => 0,
            Self::NseEq => 1,
            Self::NseFno => 2,
            Self::NseCurrency => 3,
            Self::BseEq => 4,
            Self::McxComm => 5,
            Self::BseCurrency => 7,
            Self::BseFno => 8,
        }
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Index,
            1 => Self::NseEq,
            2 => Self::NseFno,
            3 => Self::NseCurrency,
            4 => Self::BseEq,
            5 => Self::McxComm,
            7 => Self::BseCurrency,
            8 => Self::BseFno,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Index => "IDX_I",
            Self::NseEq => "NSE_EQ",
            Self::NseFno => "NSE_FNO",
            Self::NseCurrency => "NSE_CURRENCY",
            Self::BseEq => "BSE_EQ",
            Self::McxComm => "MCX_COMM",
            Self::BseCurrency => "BSE_CURRENCY",
            Self::BseFno => "BSE_FNO",
        }
    }
}

impl fmt::Display for ExchangeSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExchangeSegment {
    type Err = DhanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|seg| seg.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| DhanError::InvalidRequest(format!("unknown exchange segment {s:?}")))
    }
}

/// Dhan's numeric security ID. Sent as a string in REST and WebSocket JSON,
/// and as a 32-bit integer in the binary feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecurityId(pub u32);

impl fmt::Display for SecurityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<u32> for SecurityId {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl FromStr for SecurityId {
    type Err = DhanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim()
            .parse::<u32>()
            .map(SecurityId)
            .map_err(|_| DhanError::InvalidRequest(format!("invalid security id {s:?}")))
    }
}

impl Serialize for SecurityId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecurityId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = SecurityId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a numeric security id as a string or integer")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<SecurityId, E> {
                u32::try_from(v).map(SecurityId).map_err(E::custom)
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<SecurityId, E> {
                u32::try_from(v).map(SecurityId).map_err(E::custom)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<SecurityId, E> {
                v.trim().parse::<u32>().map(SecurityId).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

/// One tradable instrument: where it trades and its ID there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstrumentId {
    pub segment: ExchangeSegment,
    pub security_id: SecurityId,
}

impl InstrumentId {
    pub const fn new(segment: ExchangeSegment, security_id: u32) -> Self {
        Self {
            segment,
            security_id: SecurityId(security_id),
        }
    }

    pub const fn nse_eq(security_id: u32) -> Self {
        Self::new(ExchangeSegment::NseEq, security_id)
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.segment, self.security_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransactionType {
    Buy,
    Sell,
}

impl TransactionType {
    pub const fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ProductType {
    /// Delivery (cash & carry).
    #[serde(rename = "CNC")]
    Cnc,
    /// Intraday (MIS) for equity and F&O.
    #[serde(rename = "INTRADAY")]
    Intraday,
    /// Carry-forward F&O.
    #[serde(rename = "MARGIN")]
    Margin,
    /// Margin trading facility.
    #[serde(rename = "MTF")]
    Mtf,
    /// Cover order (intraday only).
    #[serde(rename = "CO")]
    Co,
    /// Bracket order (intraday only).
    #[serde(rename = "BO")]
    Bo,
    #[default]
    #[serde(other, rename = "UNKNOWN")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderType {
    Limit,
    Market,
    /// Stop-loss limit (SL-L).
    StopLoss,
    /// Stop-loss market (SL-M).
    StopLossMarket,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Validity {
    Day,
    /// Immediate or cancel.
    Ioc,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderStatus {
    /// Did not reach the exchange yet.
    Transit,
    /// Open at the exchange, awaiting execution.
    Pending,
    /// Super order: entry and one exit leg done.
    Closed,
    /// Super order: target or stop-loss leg triggered.
    Triggered,
    Rejected,
    Cancelled,
    PartTraded,
    Traded,
    Expired,
    #[default]
    #[serde(other)]
    Unknown,
}

impl OrderStatus {
    /// No further fills or state changes can happen.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Cancelled | Self::Traded | Self::Expired | Self::Closed
        )
    }

    /// The order may still fill (fully or further).
    pub const fn is_open(self) -> bool {
        matches!(
            self,
            Self::Transit | Self::Pending | Self::PartTraded | Self::Triggered
        )
    }

    /// Parses the free-form status strings used by the order-update stream
    /// (`"Cancelled"`, `"Part Traded"`, `"TRADED"`, ...).
    pub fn from_loose(s: &str) -> Self {
        let norm = s.trim().to_ascii_uppercase().replace([' ', '-'], "_");
        match norm.as_str() {
            "TRANSIT" => Self::Transit,
            "PENDING" | "OPEN" => Self::Pending,
            "CLOSED" => Self::Closed,
            "TRIGGERED" => Self::Triggered,
            "REJECTED" => Self::Rejected,
            "CANCELLED" | "CANCELED" => Self::Cancelled,
            "PART_TRADED" | "PARTIALLY_TRADED" | "PARTTRADED" => Self::PartTraded,
            "TRADED" | "COMPLETE" | "EXECUTED" => Self::Traded,
            "EXPIRED" => Self::Expired,
            _ => Self::Unknown,
        }
    }
}

/// When an after-market order is sent to the exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AmoTime {
    PreOpen,
    Open,
    #[serde(rename = "OPEN_30")]
    Open30,
    #[serde(rename = "OPEN_60")]
    Open60,
}

/// Leg of a bracket, cover or super order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LegName {
    EntryLeg,
    TargetLeg,
    StopLossLeg,
}

impl LegName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EntryLeg => "ENTRY_LEG",
            Self::TargetLeg => "TARGET_LEG",
            Self::StopLossLeg => "STOP_LOSS_LEG",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PositionType {
    Long,
    Short,
    Closed,
    #[default]
    #[serde(other)]
    Unknown,
}

/// Instrument class (the Annexure "Instrument" table), used by historical data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstrumentKind {
    Index,
    Futidx,
    Optidx,
    Equity,
    Futstk,
    Optstk,
    Futcom,
    Optfut,
    Futcur,
    Optcur,
}

impl FromStr for InstrumentKind {
    type Err = DhanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_uppercase()))
            .map_err(|_| DhanError::InvalidRequest(format!("unknown instrument kind {s:?}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OptionType {
    Call,
    Put,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_codes_round_trip() {
        for seg in ExchangeSegment::ALL {
            assert_eq!(ExchangeSegment::from_code(seg.code()), Some(seg));
            assert_eq!(seg.as_str().parse::<ExchangeSegment>().ok(), Some(seg));
        }
        assert_eq!(ExchangeSegment::from_code(6), None);
    }

    #[test]
    fn unknown_enum_values_do_not_fail() -> Result<(), serde_json::Error> {
        let s: OrderStatus = serde_json::from_str(r#""SOMETHING_NEW""#)?;
        assert_eq!(s, OrderStatus::Unknown);
        let p: ProductType = serde_json::from_str(r#""INTRADAY""#)?;
        assert_eq!(p, ProductType::Intraday);
        Ok(())
    }

    #[test]
    fn security_id_accepts_string_and_number() -> Result<(), serde_json::Error> {
        let a: SecurityId = serde_json::from_str(r#""11536""#)?;
        let b: SecurityId = serde_json::from_str("11536")?;
        assert_eq!(a, b);
        assert_eq!(serde_json::to_string(&a)?, r#""11536""#);
        Ok(())
    }

    #[test]
    fn loose_status_parsing() {
        assert_eq!(OrderStatus::from_loose("Cancelled"), OrderStatus::Cancelled);
        assert_eq!(
            OrderStatus::from_loose("Part Traded"),
            OrderStatus::PartTraded
        );
        assert_eq!(OrderStatus::from_loose("TRADED"), OrderStatus::Traded);
    }
}
