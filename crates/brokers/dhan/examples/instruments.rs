//! Downloads Dhan's instrument master and looks up a few NSE stocks.
//! Needs no credentials.
//!
//!     cargo run -p dhan --example instruments -- RELIANCE HDFCBANK INFY

use dhan::instruments::InstrumentMaster;

#[tokio::main]
async fn main() -> dhan::Result<()> {
    let symbols: Vec<String> = std::env::args().skip(1).collect();
    let symbols = if symbols.is_empty() {
        vec!["RELIANCE".into(), "HDFCBANK".into(), "INFY".into()]
    } else {
        symbols
    };

    let started = std::time::Instant::now();
    let master = InstrumentMaster::download(&reqwest::Client::new()).await?;
    println!(
        "loaded {} instruments in {:?}",
        master.len(),
        started.elapsed()
    );

    for s in &symbols {
        match master.nse_equity(s) {
            Some(i) => println!(
                "{:<12} id={:<6} series={:<3} tick=₹{} lot={} circuit=₹{}–₹{} mtf={}x asm/gsm={}",
                i.trading_symbol(),
                i.security_id,
                i.series.as_deref().unwrap_or("-"),
                i.tick_size(),
                i.lot_size,
                i.lower_circuit,
                i.upper_circuit,
                i.mtf_leverage.round_dp(2),
                i.is_under_surveillance(),
            ),
            None => println!("{s:<12} not found on NSE"),
        }
    }
    Ok(())
}
