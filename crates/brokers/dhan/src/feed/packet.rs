//! Zero-allocation parser for Dhan's binary market-feed packets.
//!
//! Every packet starts with an 8-byte header (little endian):
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 1 | response code |
//! | 1 | 2 | message length |
//! | 3 | 1 | exchange segment |
//! | 4 | 4 | security id |
//!
//! followed by a payload whose layout depends on the response code. One
//! WebSocket message may carry several packets back to back.
//!
//! Prices are `f32` on the wire and kept that way here so parsing stays
//! cheap; convert with [`to_price`] where exact decimals are needed.

use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

use crate::{ExchangeSegment, InstrumentId, SecurityId};

pub const HEADER_LEN: usize = 8;

/// Response codes (Annexure → Feed Response Code).
pub mod code {
    pub const INDEX: u8 = 1;
    pub const TICKER: u8 = 2;
    pub const QUOTE: u8 = 4;
    pub const OI: u8 = 5;
    pub const PREV_CLOSE: u8 = 6;
    pub const MARKET_STATUS: u8 = 7;
    pub const FULL: u8 = 8;
    pub const DISCONNECT: u8 = 50;
}

const TICKER_LEN: usize = 16;
const QUOTE_LEN: usize = 50;
const OI_LEN: usize = 12;
const PREV_CLOSE_LEN: usize = 16;
const FULL_LEN: usize = 162;
const DISCONNECT_LEN: usize = 10;
const DEPTH_LEVELS: usize = 5;
const DEPTH_LEVEL_LEN: usize = 20;

/// Expected packet size for a response code, if known.
pub const fn packet_len(code: u8) -> Option<usize> {
    match code {
        code::INDEX | code::TICKER => Some(TICKER_LEN),
        code::QUOTE => Some(QUOTE_LEN),
        code::OI => Some(OI_LEN),
        code::PREV_CLOSE => Some(PREV_CLOSE_LEN),
        code::MARKET_STATUS => Some(HEADER_LEN),
        code::FULL => Some(FULL_LEN),
        code::DISCONNECT => Some(DISCONNECT_LEN),
        _ => None,
    }
}

/// Converts a wire price to a decimal with 2 places (paise precision).
pub fn to_price(v: f32) -> Decimal {
    Decimal::from_f32(v)
        .map(|d| d.round_dp(2))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub response_code: u8,
    pub message_length: u16,
    /// Raw segment code; see [`Header::segment`].
    pub segment_code: u8,
    pub security_id: u32,
}

impl Header {
    pub fn segment(&self) -> Option<ExchangeSegment> {
        ExchangeSegment::from_code(self.segment_code)
    }

    pub fn instrument(&self) -> Option<InstrumentId> {
        self.segment().map(|segment| InstrumentId {
            segment,
            security_id: SecurityId(self.security_id),
        })
    }
}

/// LTP + last trade time. Sent in ticker mode (and for indices).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ticker {
    pub header: Header,
    pub ltp: f32,
    /// Last trade time as sent by Dhan (epoch seconds).
    pub ltt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    pub header: Header,
    pub ltp: f32,
    pub ltq: u16,
    pub ltt: u32,
    /// Average traded price (day VWAP).
    pub atp: f32,
    pub volume: u32,
    pub total_sell_qty: u32,
    pub total_buy_qty: u32,
    pub open: f32,
    /// Only meaningful after market close.
    pub close: f32,
    pub high: f32,
    pub low: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DepthLevel {
    pub bid_qty: u32,
    pub ask_qty: u32,
    pub bid_orders: u16,
    pub ask_orders: u16,
    pub bid_price: f32,
    pub ask_price: f32,
}

/// Quote + OI + 5-level depth in one packet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Full {
    pub header: Header,
    pub ltp: f32,
    pub ltq: u16,
    pub ltt: u32,
    pub atp: f32,
    pub volume: u32,
    pub total_sell_qty: u32,
    pub total_buy_qty: u32,
    pub oi: u32,
    pub oi_day_high: u32,
    pub oi_day_low: u32,
    pub open: f32,
    pub close: f32,
    pub high: f32,
    pub low: f32,
    pub depth: [DepthLevel; DEPTH_LEVELS],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenInterest {
    pub header: Header,
    pub oi: u32,
}

/// Sent once on subscribe with the previous session's close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrevClose {
    pub header: Header,
    pub prev_close: f32,
    pub prev_oi: u32,
}

// Kept unboxed on purpose: parsing allocates nothing, and copying ~170
// bytes is cheaper than a heap allocation per tick.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeedPacket {
    Index(Ticker),
    Ticker(Ticker),
    Quote(Quote),
    OpenInterest(OpenInterest),
    PrevClose(PrevClose),
    MarketStatus(Header),
    Full(Full),
    /// Server is closing the connection; `reason` is a Data API error code
    /// (e.g. 805 = too many connections, 807 = token expired).
    Disconnect {
        header: Header,
        reason: u16,
    },
}

impl FeedPacket {
    pub fn header(&self) -> &Header {
        match self {
            FeedPacket::Index(t) | FeedPacket::Ticker(t) => &t.header,
            FeedPacket::Quote(q) => &q.header,
            FeedPacket::OpenInterest(o) => &o.header,
            FeedPacket::PrevClose(p) => &p.header,
            FeedPacket::MarketStatus(h) => h,
            FeedPacket::Full(f) => &f.header,
            FeedPacket::Disconnect { header, .. } => header,
        }
    }

    pub fn instrument(&self) -> Option<InstrumentId> {
        self.header().instrument()
    }

    /// Last traded price, for packets that carry one.
    pub fn ltp(&self) -> Option<f32> {
        match self {
            FeedPacket::Index(t) | FeedPacket::Ticker(t) => Some(t.ltp),
            FeedPacket::Quote(q) => Some(q.ltp),
            FeedPacket::Full(f) => Some(f.ltp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    #[error("packet with code {code} needs {expected} bytes, only {available} left")]
    Truncated {
        code: u8,
        expected: usize,
        available: usize,
    },
    #[error("unknown response code {code} with unusable length {length}")]
    UnknownCode { code: u8, length: u16 },
}

/// Bounds-checked little-endian reads over one packet.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn bytes<const N: usize>(&self, at: usize) -> [u8; N] {
        // Callers check the packet length up front, so this never falls
        // back in practice; zeros keep it panic-free regardless.
        self.0
            .get(at..at + N)
            .and_then(|s| <[u8; N]>::try_from(s).ok())
            .unwrap_or([0; N])
    }
    fn u8(&self, at: usize) -> u8 {
        self.0.get(at).copied().unwrap_or(0)
    }
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.bytes(at))
    }
    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.bytes(at))
    }
    fn f32(&self, at: usize) -> f32 {
        f32::from_le_bytes(self.bytes(at))
    }
}

fn header(r: &Reader<'_>) -> Header {
    Header {
        response_code: r.u8(0),
        message_length: r.u16(1),
        segment_code: r.u8(3),
        security_id: r.u32(4),
    }
}

fn ticker(r: &Reader<'_>) -> Ticker {
    Ticker {
        header: header(r),
        ltp: r.f32(8),
        ltt: r.u32(12),
    }
}

fn quote(r: &Reader<'_>) -> Quote {
    Quote {
        header: header(r),
        ltp: r.f32(8),
        ltq: r.u16(12),
        ltt: r.u32(14),
        atp: r.f32(18),
        volume: r.u32(22),
        total_sell_qty: r.u32(26),
        total_buy_qty: r.u32(30),
        open: r.f32(34),
        close: r.f32(38),
        high: r.f32(42),
        low: r.f32(46),
    }
}

fn full(r: &Reader<'_>) -> Full {
    let mut depth = [DepthLevel::default(); DEPTH_LEVELS];
    for (i, level) in depth.iter_mut().enumerate() {
        let base = 62 + i * DEPTH_LEVEL_LEN;
        *level = DepthLevel {
            bid_qty: r.u32(base),
            ask_qty: r.u32(base + 4),
            bid_orders: r.u16(base + 8),
            ask_orders: r.u16(base + 10),
            bid_price: r.f32(base + 12),
            ask_price: r.f32(base + 16),
        };
    }
    Full {
        header: header(r),
        ltp: r.f32(8),
        ltq: r.u16(12),
        ltt: r.u32(14),
        atp: r.f32(18),
        volume: r.u32(22),
        total_sell_qty: r.u32(26),
        total_buy_qty: r.u32(30),
        oi: r.u32(34),
        oi_day_high: r.u32(38),
        oi_day_low: r.u32(42),
        open: r.f32(46),
        close: r.f32(50),
        high: r.f32(54),
        low: r.f32(58),
        depth,
    }
}

/// Parses one packet from the start of `buf`. Returns the packet (or `None`
/// for an unknown code that was safely skipped) and the bytes consumed.
fn parse_one(buf: &[u8]) -> Result<(Option<FeedPacket>, usize), PacketError> {
    let available = buf.len();
    let code = buf.first().copied().unwrap_or(0);
    if available < HEADER_LEN {
        return Err(PacketError::Truncated {
            code,
            expected: HEADER_LEN,
            available,
        });
    }
    let Some(len) = packet_len(code) else {
        // Unknown packet type: skip it using the header's length if that
        // looks sane, otherwise give up on the rest of the message.
        let length = Reader(buf).u16(1);
        let skip = usize::from(length);
        if skip >= HEADER_LEN && skip <= available {
            return Ok((None, skip));
        }
        return Err(PacketError::UnknownCode { code, length });
    };
    if available < len {
        return Err(PacketError::Truncated {
            code,
            expected: len,
            available,
        });
    }
    let r = Reader(buf.get(..len).unwrap_or(buf));
    let packet = match code {
        code::INDEX => FeedPacket::Index(ticker(&r)),
        code::TICKER => FeedPacket::Ticker(ticker(&r)),
        code::QUOTE => FeedPacket::Quote(quote(&r)),
        code::OI => FeedPacket::OpenInterest(OpenInterest {
            header: header(&r),
            oi: r.u32(8),
        }),
        code::PREV_CLOSE => FeedPacket::PrevClose(PrevClose {
            header: header(&r),
            prev_close: r.f32(8),
            prev_oi: r.u32(12),
        }),
        code::MARKET_STATUS => FeedPacket::MarketStatus(header(&r)),
        code::FULL => FeedPacket::Full(full(&r)),
        code::DISCONNECT => FeedPacket::Disconnect {
            header: header(&r),
            reason: r.u16(8),
        },
        _ => return Ok((None, len)),
    };
    Ok((Some(packet), len))
}

/// Iterator over all packets in one WebSocket message. Stops after the
/// first error, since the remaining bytes cannot be framed reliably.
#[derive(Debug)]
pub struct Packets<'a> {
    buf: &'a [u8],
    failed: bool,
}

impl Iterator for Packets<'_> {
    type Item = Result<FeedPacket, PacketError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.failed && !self.buf.is_empty() {
            match parse_one(self.buf) {
                Ok((packet, used)) => {
                    self.buf = self.buf.get(used..).unwrap_or_default();
                    if let Some(p) = packet {
                        return Some(Ok(p));
                    }
                }
                Err(e) => {
                    self.failed = true;
                    return Some(Err(e));
                }
            }
        }
        None
    }
}

/// Parses every packet in a binary WebSocket message.
pub fn parse_packets(buf: &[u8]) -> Packets<'_> {
    Packets { buf, failed: false }
}
