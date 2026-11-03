//! Instrument master parsing, using real rows from Dhan's detailed CSV.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use dhan::instruments::InstrumentMaster;
use dhan::{Decimal, ExchangeSegment, InstrumentId, InstrumentKind, SecurityId};

const CSV: &str = include_str!("fixtures_instruments.csv");

fn master() -> InstrumentMaster {
    InstrumentMaster::from_reader(CSV.as_bytes()).unwrap()
}

#[test]
fn loads_all_rows() {
    assert_eq!(master().len(), 5);
}

#[test]
fn finds_nse_equities_by_symbol() {
    let m = master();
    let rel = m.nse_equity("reliance").unwrap();
    assert_eq!(rel.security_id, SecurityId(2885));
    assert_eq!(rel.id(), Some(InstrumentId::nse_eq(2885)));
    assert_eq!(rel.kind(), Some(InstrumentKind::Equity));
    assert_eq!(rel.series.as_deref(), Some("EQ"));
    assert_eq!(rel.lot_size, Decimal::ONE);

    // Same symbol on BSE is a different instrument.
    let bse = m.find(ExchangeSegment::BseEq, "RELIANCE").unwrap();
    assert_eq!(bse.security_id, SecurityId(500325));
}

#[test]
fn tick_size_is_converted_from_paise() {
    let m = master();
    assert_eq!(
        m.nse_equity("HDFCBANK").unwrap().tick_size(),
        Decimal::new(5, 2)
    );
    assert_eq!(
        m.nse_equity("INFY").unwrap().tick_size(),
        Decimal::new(10, 2)
    );
    // Index ticks are already in rupees.
    let nifty = m
        .get(InstrumentId::new(ExchangeSegment::Index, 13))
        .unwrap();
    assert_eq!(nifty.tick_size(), Decimal::new(5, 2));
}

#[test]
fn rounds_prices_to_valid_ticks() {
    let m = master();
    let infy = m.nse_equity("INFY").unwrap(); // tick ₹0.10
    assert_eq!(
        infy.round_to_tick(Decimal::new(150_017, 2), true),
        Decimal::new(150_010, 2)
    );
    assert_eq!(
        infy.round_to_tick(Decimal::new(150_011, 2), false),
        Decimal::new(150_020, 2)
    );
    assert_eq!(
        infy.round_to_tick(Decimal::new(150_020, 2), true),
        Decimal::new(150_020, 2)
    );
}

#[test]
fn reads_circuit_limits_and_flags() {
    let m = master();
    let hdfc = m.nse_equity("HDFCBANK").unwrap();
    assert!(hdfc.upper_circuit > hdfc.lower_circuit);
    assert!(hdfc.lower_circuit > Decimal::ZERO);
    assert!(!hdfc.is_under_surveillance());
    assert!(hdfc.mtf_leverage > Decimal::ONE);
}

#[test]
fn resolve_reports_every_unknown_symbol() {
    let m = master();
    assert_eq!(
        m.resolve_nse_equities(["INFY", "HDFCBANK"]).unwrap().len(),
        2
    );
    let err = m
        .resolve_nse_equities(["INFY", "NOPE1", "NOPE2"])
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("NOPE1") && msg.contains("NOPE2"));
}

#[test]
fn rejects_a_file_in_the_wrong_format() {
    assert!(InstrumentMaster::from_reader("a,b,c\n1,2,3\n".as_bytes()).is_err());
}
