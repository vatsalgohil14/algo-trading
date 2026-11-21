//! Prints live order updates for the account until Ctrl-C. Places nothing;
//! place an order from the Dhan app to see updates arrive.
//!
//!     DHAN_CLIENT_ID=... DHAN_ACCESS_TOKEN=... cargo run -p dhan --example order_updates

use dhan::DhanClient;
use dhan::order_update::{OrderUpdateConfig, OrderUpdateEvent, OrderUpdateStream};

#[tokio::main]
async fn main() -> dhan::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let client = DhanClient::from_env()?;
    let (stream, mut events) = OrderUpdateStream::spawn(OrderUpdateConfig::from_client(&client))?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ev = events.recv() => match ev {
                None => break,
                Some(OrderUpdateEvent::Order(u)) => println!(
                    "{} {:?} {:?} {} x{} filled {} @ ₹{} | {:?} | {}",
                    u.order_no, u.order_status(), u.side(), u.symbol, u.quantity,
                    u.traded_qty, u.avg_traded_price, u.correlation_id,
                    u.reason_description.as_deref().unwrap_or("")
                ),
                Some(other) => println!("{other:?}"),
            },
        }
    }
    stream.shutdown().await;
    Ok(())
}
