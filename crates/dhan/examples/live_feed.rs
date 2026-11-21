//! Streams live quotes for every stock in a universe file and prints a
//! per-stock summary. Requires an active Dhan Data API subscription.
//!
//!     export DHAN_CLIENT_ID=...  DHAN_PIN=...  DHAN_TOTP_SECRET=...
//!     cargo run -p dhan --example live_feed -- [universe.csv] [seconds]
//!
//! Defaults: `data/universe_nse.csv` (written by `dhan-daily`) and 60 seconds.
//! Logs in on its own: with `DHAN_PIN` and `DHAN_TOTP_SECRET` set it reuses
//! or renews the cached token; otherwise it reads `DHAN_ACCESS_TOKEN`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Utc};
use dhan::InstrumentId;
use dhan::feed::packet::to_price;
use dhan::feed::{FeedEvent, FeedMode, FeedPacket, MarketFeed, MarketFeedConfig};
use dhan::token::connect_from_env;

/// Dhan sends trade times as IST clock time written as if it were UTC.
const DHAN_LTT_OFFSET_SECS: i64 = 5 * 3600 + 1800;

#[derive(Default)]
struct Seen {
    symbol: String,
    packets: u64,
    first_ltp: f32,
    ltp: f32,
    volume: u32,
    ltt: u32,
    day: (f32, f32, f32),
}

fn ist(epoch: i64) -> String {
    FixedOffset::east_opt(5 * 3600 + 1800)
        .zip(DateTime::<Utc>::from_timestamp(epoch, 0))
        .map(|(ist, t)| t.with_timezone(&ist).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "?".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "data/universe_nse.csv".into());
    let seconds: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(60);

    // `symbol,security_id,...` with a header line.
    let mut seen: BTreeMap<u32, Seen> = BTreeMap::new();
    for line in std::fs::read_to_string(&path)?.lines().skip(1) {
        let mut cols = line.split(',');
        let (Some(symbol), Some(id)) = (cols.next(), cols.next()) else {
            continue;
        };
        let id: u32 = id.trim().parse()?;
        seen.insert(
            id,
            Seen {
                symbol: symbol.trim().to_owned(),
                ..Seen::default()
            },
        );
    }
    if seen.is_empty() {
        return Err(format!("{path}: no instruments").into());
    }

    let client = connect_from_env().await?;
    let (feed, mut events) = MarketFeed::spawn(MarketFeedConfig::from_client(&client))?;
    let ids: Vec<InstrumentId> = seen.keys().map(|&id| InstrumentId::nse_eq(id)).collect();
    feed.subscribe(FeedMode::Quote, ids)?;
    println!("subscribed to {} stocks for {seconds}s", seen.len());

    let start = Instant::now();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    tick.tick().await;
    let (mut quotes, mut other, mut parse_errors, mut disconnects) = (0u64, 0u64, 0u64, 0u64);
    loop {
        tokio::select! {
            () = &mut deadline => break,
            _ = tick.tick() => {
                let live = seen.values().filter(|s| s.packets > 0).count();
                println!(
                    "{:>4}s  quotes {quotes:>7}  stocks with data {live}/{}",
                    start.elapsed().as_secs(), seen.len()
                );
            }
            ev = events.recv() => match ev {
                None => break,
                Some(FeedEvent::Connected) => println!("connected"),
                Some(FeedEvent::Disconnected { reason }) => {
                    disconnects += 1;
                    println!("disconnected: {reason}");
                }
                Some(FeedEvent::ParseError(e)) => {
                    parse_errors += 1;
                    println!("parse error: {e}");
                }
                Some(FeedEvent::Packet(FeedPacket::Quote(q))) => {
                    quotes += 1;
                    if let Some(s) = seen.get_mut(&q.header.security_id) {
                        if s.packets == 0 {
                            s.first_ltp = q.ltp;
                        }
                        s.packets += 1;
                        s.ltp = q.ltp;
                        s.volume = q.volume;
                        s.ltt = q.ltt;
                        s.day = (q.open, q.high, q.low);
                    }
                }
                Some(FeedEvent::Packet(_)) => other += 1,
            },
        }
    }
    feed.shutdown().await;

    println!(
        "\n{:<12} {:>7} {:>10} {:>10} {:>12} {:>9} {:>10} {:>10} {:>10}",
        "symbol", "quotes", "first", "last", "volume", "ltt", "day open", "day high", "day low"
    );
    for s in seen.values() {
        println!(
            "{:<12} {:>7} {:>10} {:>10} {:>12} {:>9} {:>10} {:>10} {:>10}",
            s.symbol,
            s.packets,
            to_price(s.first_ltp),
            to_price(s.ltp),
            s.volume,
            if s.ltt == 0 {
                "-".into()
            } else {
                // Dhan sends IST clock time written as if it were UTC.
                ist(i64::from(s.ltt) - DHAN_LTT_OFFSET_SECS)
            },
            to_price(s.day.0),
            to_price(s.day.1),
            to_price(s.day.2),
        );
    }
    let silent: Vec<_> = seen
        .values()
        .filter(|s| s.packets == 0)
        .map(|s| s.symbol.as_str())
        .collect();
    println!(
        "\n{quotes} quotes, {other} other packets, {parse_errors} parse errors, {disconnects} disconnects"
    );
    println!("now {} IST", ist(Utc::now().timestamp()));
    if silent.is_empty() {
        println!("every stock sent data");
    } else {
        println!("no data from {}: {}", silent.len(), silent.join(", "));
    }
    Ok(())
}
