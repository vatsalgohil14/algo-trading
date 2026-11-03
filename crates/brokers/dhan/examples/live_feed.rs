//! Streams live quotes for a few NSE stocks for 60 seconds.
//! Requires an active Dhan Data API subscription.
//!
//!     DHAN_CLIENT_ID=... DHAN_ACCESS_TOKEN=... cargo run -p dhan --example live_feed

use std::time::Duration;

use dhan::DhanClient;
use dhan::InstrumentId;
use dhan::feed::packet::to_price;
use dhan::feed::{FeedEvent, FeedMode, FeedPacket, MarketFeed, MarketFeedConfig};

#[tokio::main]
async fn main() -> dhan::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let client = DhanClient::from_env()?;
    let (feed, mut events) = MarketFeed::spawn(MarketFeedConfig::from_client(&client))?;

    // RELIANCE, HDFCBANK, INFY on NSE.
    feed.subscribe(
        FeedMode::Quote,
        vec![
            InstrumentId::nse_eq(2885),
            InstrumentId::nse_eq(1333),
            InstrumentId::nse_eq(1594),
        ],
    )?;

    let deadline = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => break,
            ev = events.recv() => match ev {
                None => break,
                Some(FeedEvent::Packet(FeedPacket::Quote(q))) => println!(
                    "{:>6} ltp ₹{:<9} vwap ₹{:<9} vol {:>10} H {} L {}",
                    q.header.security_id, to_price(q.ltp), to_price(q.atp), q.volume,
                    to_price(q.high), to_price(q.low)
                ),
                Some(FeedEvent::Packet(FeedPacket::PrevClose(p))) => {
                    println!("{:>6} prev close ₹{}", p.header.security_id, to_price(p.prev_close));
                }
                Some(other) => println!("{other:?}"),
            },
        }
    }
    feed.shutdown().await;
    Ok(())
}
