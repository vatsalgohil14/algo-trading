//! Gets or reuses a Dhan access token, confirms it works and says when it
//! expires. Never prints the token. See `docs/runbook/dhan-token.md`.

use std::process::ExitCode;

use chrono::FixedOffset;
use dhan::token::TokenKeeper;

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (client, token) = TokenKeeper::from_env()?.connect().await?;
    client.profile().await?;
    let ist = FixedOffset::east_opt(5 * 3600 + 30 * 60).ok_or("bad offset")?;
    println!(
        "Token works for client {}. Expires {} IST.",
        client.client_id(),
        token
            .expires
            .with_timezone(&ist)
            .format("%Y-%m-%d %H:%M:%S")
    );
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
