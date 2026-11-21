//! Binary market-feed packets, built byte by byte from Dhan's spec.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use dhan::feed::packet::{FeedPacket, PacketError, code, parse_packets, to_price};
use dhan::{Decimal, ExchangeSegment, InstrumentId};

/// Little-endian packet builder.
struct P(Vec<u8>);

impl P {
    fn header(code: u8, len: u16, segment: u8, security_id: u32) -> Self {
        let mut v = vec![code];
        v.extend(len.to_le_bytes());
        v.push(segment);
        v.extend(security_id.to_le_bytes());
        P(v)
    }
    fn f32(mut self, x: f32) -> Self {
        self.0.extend(x.to_le_bytes());
        self
    }
    fn u32(mut self, x: u32) -> Self {
        self.0.extend(x.to_le_bytes());
        self
    }
    fn u16(mut self, x: u16) -> Self {
        self.0.extend(x.to_le_bytes());
        self
    }
}

fn ticker(security_id: u32, ltp: f32, ltt: u32) -> Vec<u8> {
    P::header(code::TICKER, 16, 1, security_id)
        .f32(ltp)
        .u32(ltt)
        .0
}

fn quote(security_id: u32) -> Vec<u8> {
    P::header(code::QUOTE, 50, 1, security_id)
        .f32(2450.5) // ltp
        .u16(25) // ltq
        .u32(1_730_000_000) // ltt
        .f32(2448.25) // atp
        .u32(1_234_567) // volume
        .u32(50_000) // total sell
        .u32(60_000) // total buy
        .f32(2440.0) // open
        .f32(0.0) // close
        .f32(2460.0) // high
        .f32(2435.1) // low
        .0
}

fn full(security_id: u32) -> Vec<u8> {
    let mut p = P::header(code::FULL, 162, 1, security_id)
        .f32(1428.05)
        .u16(10)
        .u32(1_730_000_100)
        .f32(1427.5)
        .u32(999)
        .u32(11)
        .u32(22)
        .u32(0)
        .u32(0)
        .u32(0)
        .f32(1420.0)
        .f32(0.0)
        .f32(1430.0)
        .f32(1418.0);
    for level in 0..5u32 {
        p = p
            .u32(100 + level) // bid qty
            .u32(200 + level) // ask qty
            .u16(3) // bid orders
            .u16(4) // ask orders
            .f32(1428.0 - level as f32 * 0.05)
            .f32(1428.1 + level as f32 * 0.05);
    }
    p.0
}

#[test]
fn parses_ticker() {
    let buf = ticker(2885, 1428.05, 1_730_000_000);
    let packets: Vec<_> = parse_packets(&buf).collect();
    assert_eq!(packets.len(), 1);
    let FeedPacket::Ticker(t) = packets[0].clone().unwrap() else {
        panic!("expected ticker")
    };
    assert_eq!(t.header.security_id, 2885);
    assert_eq!(t.header.segment(), Some(ExchangeSegment::NseEq));
    assert_eq!(t.ltt, 1_730_000_000);
    assert_eq!(to_price(t.ltp), Decimal::new(142_805, 2));
    assert_eq!(
        packets[0].as_ref().unwrap().instrument(),
        Some(InstrumentId::nse_eq(2885))
    );
}

#[test]
fn parses_quote() {
    let buf = quote(1333);
    assert_eq!(buf.len(), 50);
    let FeedPacket::Quote(q) = parse_packets(&buf).next().unwrap().unwrap() else {
        panic!()
    };
    assert_eq!(q.ltq, 25);
    assert_eq!(q.volume, 1_234_567);
    assert_eq!(q.total_buy_qty, 60_000);
    assert_eq!(to_price(q.atp), Decimal::new(244_825, 2));
    assert_eq!(to_price(q.low), Decimal::new(243_510, 2));
}

#[test]
fn parses_full_with_depth() {
    let buf = full(2885);
    assert_eq!(buf.len(), 162);
    let FeedPacket::Full(f) = parse_packets(&buf).next().unwrap().unwrap() else {
        panic!()
    };
    assert_eq!(f.volume, 999);
    assert_eq!(to_price(f.high), Decimal::new(1430, 0));
    assert_eq!(f.depth[0].bid_qty, 100);
    assert_eq!(f.depth[4].ask_qty, 204);
    assert_eq!(f.depth[2].bid_orders, 3);
    assert_eq!(to_price(f.depth[1].ask_price), Decimal::new(142_815, 2));
}

#[test]
fn parses_prev_close_oi_and_disconnect() {
    let prev = P::header(code::PREV_CLOSE, 16, 1, 1333)
        .f32(1650.4)
        .u32(0)
        .0;
    let oi = P::header(code::OI, 12, 2, 49081).u32(123_456).0;
    let disc = P::header(code::DISCONNECT, 10, 0, 0).u16(805).0;
    let mut buf = prev;
    buf.extend(oi);
    buf.extend(disc);
    let parsed: Vec<_> = parse_packets(&buf).map(Result::unwrap).collect();
    assert!(
        matches!(parsed[0], FeedPacket::PrevClose(p) if to_price(p.prev_close) == Decimal::new(165_040, 2))
    );
    assert!(matches!(parsed[1], FeedPacket::OpenInterest(o) if o.oi == 123_456));
    assert!(matches!(
        parsed[2],
        FeedPacket::Disconnect { reason: 805, .. }
    ));
}

#[test]
fn handles_back_to_back_packets() {
    let mut buf = ticker(1, 10.0, 1);
    buf.extend(quote(2));
    buf.extend(full(3));
    buf.extend(ticker(4, 40.0, 4));
    let ids: Vec<u32> = parse_packets(&buf)
        .map(|p| p.unwrap().header().security_id)
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[test]
fn truncated_packet_is_an_error_not_a_panic() {
    let mut buf = ticker(1, 10.0, 1);
    let mut cut = quote(2);
    cut.truncate(30);
    buf.extend(cut);
    let results: Vec<_> = parse_packets(&buf).collect();
    assert_eq!(results.len(), 2);
    assert!(results[0].is_ok());
    assert_eq!(
        results[1],
        Err(PacketError::Truncated {
            code: code::QUOTE,
            expected: 50,
            available: 30
        })
    );
}

#[test]
fn unknown_code_is_skipped_when_length_is_sane() {
    let mut buf = P::header(99, 12, 1, 7).u32(0).0;
    buf.extend(ticker(5, 1.0, 1));
    let ids: Vec<u32> = parse_packets(&buf)
        .map(|p| p.unwrap().header().security_id)
        .collect();
    assert_eq!(ids, vec![5]);
}

#[test]
fn garbage_never_panics() {
    // Every prefix of a valid stream, plus random-ish bytes.
    let mut buf = ticker(1, 10.0, 1);
    buf.extend(full(2));
    for end in 0..=buf.len() {
        for _ in parse_packets(&buf[..end]) {}
    }
    let noise: Vec<u8> = (0..2000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    for start in 0..64 {
        for _ in parse_packets(&noise[start..]) {}
    }
}
