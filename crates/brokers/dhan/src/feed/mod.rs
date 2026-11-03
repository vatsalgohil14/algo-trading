//! Live market feed over Dhan's binary WebSocket.
//!
//! [`MarketFeed::spawn`] starts a background task that owns the connection.
//! The task:
//!
//! * remembers every subscription and replays them after each reconnect,
//! * reconnects with backoff when the connection drops or goes silent,
//! * sends packets to a bounded channel (the reader applies backpressure
//!   rather than silently dropping ticks),
//! * reports connection changes as [`FeedEvent`]s, so the trading engine can
//!   stop opening positions while data is stale.
//!
//! Limits: up to 5 connections per account and 5000 instruments per
//! connection; at most 100 instruments per subscribe message (handled here).

pub mod packet;

use std::collections::HashMap;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

pub use packet::{FeedPacket, PacketError, parse_packets};

use crate::ws::{self, Backoff, ReconnectPolicy, Ws};
use crate::{DhanClient, DhanError, InstrumentId, Result, Secret};

pub const MAX_INSTRUMENTS_PER_CONNECTION: usize = 5000;
const MAX_INSTRUMENTS_PER_MESSAGE: usize = 100;

/// What data to receive for an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeedMode {
    /// LTP and last trade time.
    Ticker,
    /// LTP, volume, OHLC, VWAP, buy/sell totals.
    Quote,
    /// Quote + OI + 5-level depth.
    Full,
}

impl FeedMode {
    const fn subscribe_code(self) -> u8 {
        match self {
            FeedMode::Ticker => 15,
            FeedMode::Quote => 17,
            FeedMode::Full => 21,
        }
    }

    const fn unsubscribe_code(self) -> u8 {
        match self {
            FeedMode::Ticker => 16,
            FeedMode::Quote => 18,
            FeedMode::Full => 22,
        }
    }
}

const DISCONNECT_CODE: u8 = 12;

#[derive(Debug, Clone)]
pub struct MarketFeedConfig {
    pub url: String,
    pub client_id: String,
    pub access_token: Secret,
    pub reconnect: ReconnectPolicy,
    /// Capacity of the event channel.
    pub channel_capacity: usize,
}

impl MarketFeedConfig {
    pub fn from_client(client: &DhanClient) -> Self {
        Self {
            url: client.config().market_feed_url.clone(),
            client_id: client.client_id().to_owned(),
            access_token: client.access_token(),
            reconnect: ReconnectPolicy::default(),
            channel_capacity: 65_536,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeedEvent {
    /// Connected and all known subscriptions were sent.
    Connected,
    /// Connection lost; the task is reconnecting. Treat data as stale.
    Disconnected {
        reason: String,
    },
    Packet(FeedPacket),
    /// A message could not be fully parsed. Packets before the bad one were
    /// still delivered.
    ParseError(PacketError),
}

#[derive(Debug)]
enum Command {
    Subscribe(FeedMode, Vec<InstrumentId>),
    Unsubscribe(Vec<InstrumentId>),
    UpdateToken(Secret),
    Shutdown,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct InstrumentJson {
    exchange_segment: &'static str,
    security_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SubscriptionJson {
    request_code: u8,
    instrument_count: usize,
    instrument_list: Vec<InstrumentJson>,
}

/// Builds subscribe/unsubscribe messages, at most 100 instruments each.
fn subscription_messages(request_code: u8, instruments: &[InstrumentId]) -> Vec<String> {
    instruments
        .chunks(MAX_INSTRUMENTS_PER_MESSAGE)
        .filter_map(|chunk| {
            let msg = SubscriptionJson {
                request_code,
                instrument_count: chunk.len(),
                instrument_list: chunk
                    .iter()
                    .map(|i| InstrumentJson {
                        exchange_segment: i.segment.as_str(),
                        security_id: i.security_id.to_string(),
                    })
                    .collect(),
            };
            serde_json::to_string(&msg).ok()
        })
        .collect()
}

/// Handle to the background feed task. Dropping it stops the task.
#[derive(Debug)]
pub struct MarketFeed {
    commands: mpsc::UnboundedSender<Command>,
    task: Option<JoinHandle<()>>,
}

impl MarketFeed {
    /// Starts the feed. Events arrive on the returned receiver; the
    /// connection is opened immediately and kept open until shutdown.
    pub fn spawn(config: MarketFeedConfig) -> Result<(Self, mpsc::Receiver<FeedEvent>)> {
        if config.access_token.is_empty() || config.client_id.trim().is_empty() {
            return Err(DhanError::InvalidRequest(
                "feed needs a client id and access token".into(),
            ));
        }
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::channel(config.channel_capacity.max(1));
        let task = tokio::spawn(run(config, cmd_rx, ev_tx));
        Ok((
            Self {
                commands: cmd_tx,
                task: Some(task),
            },
            ev_rx,
        ))
    }

    /// Adds instruments (or changes their mode). Takes effect immediately if
    /// connected, otherwise on the next connect.
    pub fn subscribe(
        &self,
        mode: FeedMode,
        instruments: impl Into<Vec<InstrumentId>>,
    ) -> Result<()> {
        self.send(Command::Subscribe(mode, instruments.into()))
    }

    pub fn unsubscribe(&self, instruments: impl Into<Vec<InstrumentId>>) -> Result<()> {
        self.send(Command::Unsubscribe(instruments.into()))
    }

    /// Uses a new access token from the next reconnect onward. Call this
    /// after the daily token refresh.
    pub fn update_token(&self, token: Secret) -> Result<()> {
        self.send(Command::UpdateToken(token))
    }

    /// Sends Dhan's disconnect request, closes the socket and waits for the
    /// task to finish.
    pub async fn shutdown(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    fn send(&self, cmd: Command) -> Result<()> {
        self.commands
            .send(cmd)
            .map_err(|_| DhanError::ChannelClosed)
    }
}

impl Drop for MarketFeed {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = self.commands.send(Command::Shutdown);
            // Give the task a moment to close cleanly, but never leak it.
            tokio::spawn(async move {
                let abort = task.abort_handle();
                if tokio::time::timeout(std::time::Duration::from_secs(2), task)
                    .await
                    .is_err()
                {
                    abort.abort();
                }
            });
        }
    }
}

struct State {
    config: MarketFeedConfig,
    subs: HashMap<InstrumentId, FeedMode>,
}

enum SessionEnd {
    Shutdown,
    Lost(String),
}

impl State {
    fn url(&self) -> String {
        format!(
            "{}?version=2&token={}&clientId={}&authType=2",
            self.config.url,
            self.config.access_token.expose(),
            self.config.client_id
        )
    }

    /// Applies a command to the subscription table. Returns the messages to
    /// send if connected, or `Err(())` for shutdown.
    fn apply(&mut self, cmd: Command) -> Result<Vec<String>, ()> {
        match cmd {
            Command::Shutdown => Err(()),
            Command::UpdateToken(t) => {
                self.config.access_token = t;
                Ok(Vec::new())
            }
            Command::Subscribe(mode, instruments) => {
                let mut msgs = Vec::new();
                let mut changed_mode: HashMap<FeedMode, Vec<InstrumentId>> = HashMap::new();
                let mut new: Vec<InstrumentId> = Vec::new();
                for i in instruments {
                    match self.subs.get(&i) {
                        Some(&m) if m == mode => {}
                        Some(&m) => changed_mode.entry(m).or_default().push(i),
                        None => {
                            if self.subs.len() + new.len() >= MAX_INSTRUMENTS_PER_CONNECTION {
                                tracing::error!(instrument = %i, "subscription limit of {MAX_INSTRUMENTS_PER_CONNECTION} reached; ignoring");
                                continue;
                            }
                            new.push(i);
                        }
                    }
                }
                for (old_mode, list) in changed_mode {
                    msgs.extend(subscription_messages(old_mode.unsubscribe_code(), &list));
                    new.extend(list);
                }
                for i in &new {
                    self.subs.insert(*i, mode);
                }
                msgs.extend(subscription_messages(mode.subscribe_code(), &new));
                Ok(msgs)
            }
            Command::Unsubscribe(instruments) => {
                let mut by_mode: HashMap<FeedMode, Vec<InstrumentId>> = HashMap::new();
                for i in instruments {
                    if let Some(m) = self.subs.remove(&i) {
                        by_mode.entry(m).or_default().push(i);
                    }
                }
                Ok(by_mode
                    .into_iter()
                    .flat_map(|(m, list)| subscription_messages(m.unsubscribe_code(), &list))
                    .collect())
            }
        }
    }

    fn resubscribe_messages(&self) -> Vec<String> {
        let mut by_mode: HashMap<FeedMode, Vec<InstrumentId>> = HashMap::new();
        for (i, m) in &self.subs {
            by_mode.entry(*m).or_default().push(*i);
        }
        by_mode
            .into_iter()
            .flat_map(|(m, list)| subscription_messages(m.subscribe_code(), &list))
            .collect()
    }
}

async fn run(
    config: MarketFeedConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<FeedEvent>,
) {
    let mut backoff = Backoff::new(config.reconnect);
    let mut state = State {
        config,
        subs: HashMap::new(),
    };

    loop {
        let end = match ws::connect(&state.url(), state.config.reconnect.connect_timeout).await {
            Ok(socket) => {
                backoff.reset();
                session(socket, &mut state, &mut commands, &events).await
            }
            Err(reason) => SessionEnd::Lost(reason),
        };
        let reason = match end {
            SessionEnd::Shutdown => return,
            SessionEnd::Lost(reason) => reason,
        };
        tracing::warn!(%reason, "market feed disconnected");
        if events
            .send(FeedEvent::Disconnected { reason })
            .await
            .is_err()
        {
            return;
        }

        // Wait before reconnecting, but keep accepting commands meanwhile.
        let sleep = tokio::time::sleep(backoff.next_delay());
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                cmd = commands.recv() => match cmd {
                    None => return,
                    Some(cmd) => if state.apply(cmd).is_err() { return },
                },
            }
        }
    }
}

async fn session(
    socket: Ws,
    state: &mut State,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::Sender<FeedEvent>,
) -> SessionEnd {
    let (mut sink, mut stream) = socket.split();

    for msg in state.resubscribe_messages() {
        if let Err(e) = sink.send(Message::Text(msg.into())).await {
            return SessionEnd::Lost(format!("resubscribe failed: {e}"));
        }
    }
    tracing::info!(instruments = state.subs.len(), "market feed connected");
    if events.send(FeedEvent::Connected).await.is_err() {
        return SessionEnd::Shutdown;
    }

    let idle_timeout = state.config.reconnect.idle_timeout;
    let mut last_seen = Instant::now();
    let mut watchdog = tokio::time::interval(idle_timeout / 4);

    loop {
        tokio::select! {
            msg = stream.next() => {
                last_seen = Instant::now();
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        for parsed in parse_packets(&data) {
                            let event = match parsed {
                                Ok(FeedPacket::Disconnect { reason, .. }) => {
                                    return SessionEnd::Lost(format!("server disconnect, code {reason}"));
                                }
                                Ok(p) => FeedEvent::Packet(p),
                                Err(e) => FeedEvent::ParseError(e),
                            };
                            if events.send(event).await.is_err() {
                                return SessionEnd::Shutdown;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = sink.send(Message::Pong(payload)).await {
                            return SessionEnd::Lost(format!("pong failed: {e}"));
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return SessionEnd::Lost(format!("server closed: {frame:?}"));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return SessionEnd::Lost(e.to_string()),
                    None => return SessionEnd::Lost("stream ended".into()),
                }
            }
            cmd = commands.recv() => {
                let Some(cmd) = cmd else {
                    close(&mut sink).await;
                    return SessionEnd::Shutdown;
                };
                match state.apply(cmd) {
                    Err(()) => {
                        close(&mut sink).await;
                        return SessionEnd::Shutdown;
                    }
                    Ok(msgs) => {
                        for msg in msgs {
                            if let Err(e) = sink.send(Message::Text(msg.into())).await {
                                return SessionEnd::Lost(format!("subscribe failed: {e}"));
                            }
                        }
                    }
                }
            }
            _ = watchdog.tick() => {
                if last_seen.elapsed() > idle_timeout {
                    return SessionEnd::Lost(format!("no data or ping for {idle_timeout:?}"));
                }
            }
        }
    }
}

async fn close<S>(sink: &mut S)
where
    S: futures_util::Sink<Message> + Unpin,
{
    let msg = format!(r#"{{"RequestCode":{DISCONNECT_CODE}}}"#);
    let _ = sink.send(Message::Text(msg.into())).await;
    let _ = sink.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            config: MarketFeedConfig {
                url: "wss://example.invalid".into(),
                client_id: "1".into(),
                access_token: Secret::new("t"),
                reconnect: ReconnectPolicy::default(),
                channel_capacity: 8,
            },
            subs: HashMap::new(),
        }
    }

    #[test]
    fn chunks_subscriptions_by_100() {
        let ids: Vec<_> = (1..=250).map(InstrumentId::nse_eq).collect();
        let msgs = subscription_messages(15, &ids);
        assert_eq!(msgs.len(), 3);
        assert!(
            msgs.first()
                .is_some_and(|m| m.contains(r#""InstrumentCount":100"#))
        );
        assert!(
            msgs.last()
                .is_some_and(|m| m.contains(r#""InstrumentCount":50"#))
        );
        assert!(
            msgs.first()
                .is_some_and(|m| m.contains(r#"{"ExchangeSegment":"NSE_EQ","SecurityId":"1"}"#))
        );
    }

    #[test]
    fn mode_change_unsubscribes_old_mode() -> Result<(), ()> {
        let mut s = state();
        let a = InstrumentId::nse_eq(1333);
        let first = s.apply(Command::Subscribe(FeedMode::Ticker, vec![a]))?;
        assert_eq!(first.len(), 1);
        // Same mode again: nothing to send.
        assert!(
            s.apply(Command::Subscribe(FeedMode::Ticker, vec![a]))?
                .is_empty()
        );
        // New mode: unsubscribe ticker (16), subscribe full (21).
        let change = s.apply(Command::Subscribe(FeedMode::Full, vec![a]))?;
        assert_eq!(change.len(), 2);
        assert!(
            change
                .first()
                .is_some_and(|m| m.contains(r#""RequestCode":16"#))
        );
        assert!(
            change
                .last()
                .is_some_and(|m| m.contains(r#""RequestCode":21"#))
        );
        assert_eq!(s.resubscribe_messages().len(), 1);
        let unsub = s.apply(Command::Unsubscribe(vec![a]))?;
        assert!(
            unsub
                .first()
                .is_some_and(|m| m.contains(r#""RequestCode":22"#))
        );
        assert!(s.resubscribe_messages().is_empty());
        Ok(())
    }

    #[test]
    fn url_does_not_leak_into_debug() {
        let s = state();
        assert!(!format!("{:?}", s.config).contains("token=t"));
        assert!(s.url().contains("version=2") && s.url().contains("authType=2"));
    }
}
