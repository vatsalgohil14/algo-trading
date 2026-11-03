//! Read-only health check of a Dhan account: token, funds, positions,
//! orders, static IPs. Places nothing.
//!
//!     DHAN_CLIENT_ID=... DHAN_ACCESS_TOKEN=... cargo run -p dhan --example account_check

use dhan::DhanClient;

#[tokio::main]
async fn main() -> dhan::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let client = DhanClient::from_env()?;

    let profile = client.profile().await?;
    println!(
        "client {} | token valid until {} | data plan: {} | segments: {}",
        profile.dhan_client_id, profile.token_validity, profile.data_plan, profile.active_segment
    );

    let funds = client.fund_limit().await?;
    println!(
        "available ₹{} | utilised ₹{} | start of day ₹{}",
        funds.available_balance, funds.utilized_amount, funds.sod_limit
    );

    let positions = client.positions().await?;
    let open: Vec<_> = positions.iter().filter(|p| p.is_open()).collect();
    println!("{} positions today, {} open", positions.len(), open.len());
    for p in open {
        println!(
            "  {:<12} net {:>6} | unrealised ₹{} | realised ₹{}",
            p.trading_symbol.as_deref().unwrap_or("?"),
            p.net_qty,
            p.unrealized_profit,
            p.realized_profit
        );
    }

    let orders = client.order_book().await?;
    let pending = orders.iter().filter(|o| o.order_status.is_open()).count();
    println!("{} orders today, {pending} still open", orders.len());

    match client.static_ips().await {
        Ok(ips) => println!(
            "static IP primary={} secondary={}",
            ips.primary_ip.as_deref().unwrap_or("not set"),
            ips.secondary_ip.as_deref().unwrap_or("not set")
        ),
        Err(e) => println!("static IP lookup failed: {e}"),
    }
    Ok(())
}
