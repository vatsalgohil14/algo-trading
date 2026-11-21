//! Live order updates over Dhan's JSON WebSocket.
//!
//! Every status change of every order on the account (from any platform)
//! arrives here: placed, modified, partially filled, filled, rejected,
//! cancelled. This is the fastest way for the order manager to learn about
//! fills; the order book is the fallback for reconciliation.

use chrono::NaiveDateTime;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::serde_util as su;
use crate::ws::{self, Backoff, ReconnectPolicy, Ws};
use crate::{
    DhanClient, DhanError, OrderStatus, OrderType, ProductType, Result, Secret, SecurityId,
    TransactionType,
};

/// One order-update message. Field names follow Dhan's (PascalCase).
/// Dhan's codes are kept raw; use the accessor methods for typed values.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct OrderUpdate {
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub exchange: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub segment: String,
    /// `P` for orders placed through the API.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub source: String,
    #[serde(default, deserialize_with = "su::opt_enum_lenient")]
    pub security_id: Option<SecurityId>,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub client_id: String,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub exch_order_no: Option<String>,
    /// Dhan order ID.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub order_no: String,
    /// `C` CNC, `I` intraday, `M` margin, `F` MTF, `V` CO, `B` BO.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub product: String,
    /// `B` / `S`.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub txn_type: String,
    /// `LMT`, `MKT`, `SL`, `SLM`.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub order_type: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub validity: String,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub traded_qty: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub remaining_quantity: i64,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub disc_quantity: i64,
    #[serde(default, deserialize_with = "su::decimal")]
    pub price: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub trigger_price: Decimal,
    /// Price of the latest fill.
    #[serde(default, deserialize_with = "su::decimal")]
    pub traded_price: Decimal,
    #[serde(default, deserialize_with = "su::decimal")]
    pub avg_traded_price: Decimal,
    /// Entry-leg order number, linking BO/CO exit legs to their entry.
    #[serde(default, deserialize_with = "su::opt_string")]
    pub algo_ord_no: Option<String>,
    /// `"1"` for after-market orders.
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub off_mkt_flag: String,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub order_date_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub exch_order_time: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "su::opt_datetime")]
    pub last_updated_time: Option<NaiveDateTime>,
    /// `"Super Order"` for super-order legs.
    #[serde(default, deserialize_with = "su::opt_string")]
    pub remarks: Option<String>,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub mkt_type: String,
    /// Rejection reason, or `CONFIRMED`.
    #[serde(default, deserialize_with = "su::opt_string")]
    pub reason_description: Option<String>,
    /// 1 entry, 2 stop-loss, 3 target.
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub leg_no: i64,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub instrument: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub symbol: String,
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub product_name: String,
    /// Free-form status, e.g. `"Cancelled"`, `"TRADED"`; see [`status`](Self::status).
    #[serde(default, deserialize_with = "su::string_lenient")]
    pub status: String,
    #[serde(default, deserialize_with = "su::i64_lenient")]
    pub lot_size: i64,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub isin: Option<String>,
    #[serde(default, deserialize_with = "su::opt_decimal")]
    pub ref_ltp: Option<Decimal>,
    #[serde(default, deserialize_with = "su::opt_decimal")]
    pub tick_size: Option<Decimal>,
    #[serde(default, deserialize_with = "su::opt_string")]
    pub correlation_id: Option<String>,
}

impl OrderUpdate {
    pub fn order_status(&self) -> OrderStatus {
        OrderStatus::from_loose(&self.status)
    }

    pub fn side(&self) -> Option<TransactionType> {
        match self.txn_type.trim() {
            "B" => Some(TransactionType::Buy),
            "S" => Some(TransactionType::Sell),
            _ => None,
        }
    }

    pub fn product_type(&self) -> ProductType {
        match self.product.trim() {
            "C" => ProductType::Cnc,
            "I" => ProductType::Intraday,
            "M" => ProductType::Margin,
            "F" => ProductType::Mtf,
            "V" => ProductType::Co,
            "B" => ProductType::Bo,
            _ => ProductType::Unknown,
        }
    }

    pub fn order_kind(&self) -> OrderType {
        match self.order_type.trim() {
            "LMT" => OrderType::Limit,
            "MKT" => OrderType::Market,
            "SL" => OrderType::StopLoss,
            "SLM" => OrderType::StopLossMarket,
            _ => OrderType::Unknown,
        }
    }

    pub fn is_super_order_leg(&self) -> bool {
        self.remarks
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case("super order"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrderUpdateEvent {
    /// Connected and logged in.
    Connected,
    /// Connection lost; reconnecting. Reconcile with the order book after
    /// the next `Connected`, since updates sent meanwhile are not replayed.
    Disconnected {
        reason: String,
    },
    Order(Box<OrderUpdate>),
    /// Any other message type, passed through untouched.
    Other(Value),
    ParseError(String),
}

/// Parses one text message. Dhan's payload repeats some keys with
/// different case and occasionally duplicates a key, so it goes through
/// `serde_json::Value` (last key wins) before typed decoding.
pub fn parse_message(text: &str) -> OrderUpdateEvent {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return OrderUpdateEvent::ParseError(format!("invalid JSON: {e}")),
    };
    let is_order = value
        .get("Type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("order_alert"));
    if !is_order {
        return OrderUpdateEvent::Other(value);
    }
    match value
        .get("Data")
        .cloned()
        .map(serde_json::from_value::<OrderUpdate>)
    {
        Some(Ok(update)) => OrderUpdateEvent::Order(Box::new(update)),
        Some(Err(e)) => OrderUpdateEvent::ParseError(format!("bad order update: {e}")),
        None => OrderUpdateEvent::ParseError("order_alert without Data".into()),
    }
}

#[derive(Debug, Clone)]
pub struct OrderUpdateConfig {
    pub url: String,
    pub client_id: String,
    pub access_token: Secret,
    pub reconnect: ReconnectPolicy,
    pub channel_capacity: usize,
}

impl OrderUpdateConfig {
    pub fn from_client(client: &DhanClient) -> Self {
        Self {
            url: client.config().order_update_url.clone(),
            client_id: client.client_id().to_owned(),
            access_token: client.access_token(),
            reconnect: ReconnectPolicy {
                // Order updates can be quiet for long stretches; liveness is
                // checked with our own pings instead of data.
                idle_timeout: std::time::Duration::from_secs(60),
                ..ReconnectPolicy::default()
            },
            channel_capacity: 4096,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct LoginReq<'a> {
    msg_code: u8,
    client_id: &'a str,
    token: &'a str,
}

#[derive(Serialize)]
struct Login<'a> {
    #[serde(rename = "LoginReq")]
    login_req: LoginReq<'a>,
    #[serde(rename = "UserType")]
    user_type: &'static str,
}

enum Command {
    UpdateToken(Secret),
    Shutdown,
}

/// Handle to the background order-update task. Dropping it stops the task.
#[derive(Debug)]
pub struct OrderUpdateStream {
    commands: mpsc::UnboundedSender<Command>,
    task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::UpdateToken(_) => f.write_str("UpdateToken(..)"),
            Command::Shutdown => f.write_str("Shutdown"),
        }
    }
}

impl OrderUpdateStream {
    pub fn spawn(config: OrderUpdateConfig) -> Result<(Self, mpsc::Receiver<OrderUpdateEvent>)> {
        if config.access_token.is_empty() || config.client_id.trim().is_empty() {
            return Err(DhanError::InvalidRequest(
                "order updates need a client id and access token".into(),
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

    /// Uses a new access token from the next reconnect onward.
    pub fn update_token(&self, token: Secret) -> Result<()> {
        self.commands
            .send(Command::UpdateToken(token))
            .map_err(|_| DhanError::ChannelClosed)
    }

    pub async fn shutdown(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for OrderUpdateStream {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = self.commands.send(Command::Shutdown);
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

enum SessionEnd {
    Shutdown,
    Lost(String),
}

async fn run(
    mut config: OrderUpdateConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<OrderUpdateEvent>,
) {
    let mut backoff = Backoff::new(config.reconnect);
    loop {
        let end = match ws::connect(&config.url, config.reconnect.connect_timeout).await {
            Ok(socket) => {
                backoff.reset();
                session(socket, &mut config, &mut commands, &events).await
            }
            Err(reason) => SessionEnd::Lost(reason),
        };
        let reason = match end {
            SessionEnd::Shutdown => return,
            SessionEnd::Lost(reason) => reason,
        };
        tracing::warn!(%reason, "order update stream disconnected");
        if events
            .send(OrderUpdateEvent::Disconnected { reason })
            .await
            .is_err()
        {
            return;
        }
        let sleep = tokio::time::sleep(backoff.next_delay());
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                cmd = commands.recv() => match cmd {
                    None | Some(Command::Shutdown) => return,
                    Some(Command::UpdateToken(t)) => config.access_token = t,
                },
            }
        }
    }
}

async fn session(
    socket: Ws,
    config: &mut OrderUpdateConfig,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::Sender<OrderUpdateEvent>,
) -> SessionEnd {
    let (mut sink, mut stream) = socket.split();

    let login = Login {
        login_req: LoginReq {
            msg_code: 42,
            client_id: &config.client_id,
            token: config.access_token.expose(),
        },
        user_type: "SELF",
    };
    let login = match serde_json::to_string(&login) {
        Ok(s) => s,
        Err(e) => return SessionEnd::Lost(format!("could not encode login: {e}")),
    };
    if let Err(e) = sink.send(Message::Text(login.into())).await {
        return SessionEnd::Lost(format!("login send failed: {e}"));
    }
    tracing::info!("order update stream connected");
    if events.send(OrderUpdateEvent::Connected).await.is_err() {
        return SessionEnd::Shutdown;
    }

    let idle_timeout = config.reconnect.idle_timeout;
    let mut last_seen = Instant::now();
    let mut heartbeat = tokio::time::interval(idle_timeout / 4);

    loop {
        tokio::select! {
            msg = stream.next() => {
                last_seen = Instant::now();
                let event = match msg {
                    Some(Ok(Message::Text(text))) => parse_message(&text),
                    Some(Ok(Message::Binary(bytes))) => match std::str::from_utf8(&bytes) {
                        Ok(text) => parse_message(text),
                        Err(_) => OrderUpdateEvent::ParseError("non-UTF-8 binary message".into()),
                    },
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = sink.send(Message::Pong(payload)).await {
                            return SessionEnd::Lost(format!("pong failed: {e}"));
                        }
                        continue;
                    }
                    Some(Ok(Message::Close(frame))) => return SessionEnd::Lost(format!("server closed: {frame:?}")),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return SessionEnd::Lost(e.to_string()),
                    None => return SessionEnd::Lost("stream ended".into()),
                };
                if events.send(event).await.is_err() {
                    return SessionEnd::Shutdown;
                }
            }
            cmd = commands.recv() => match cmd {
                None | Some(Command::Shutdown) => {
                    let _ = sink.close().await;
                    return SessionEnd::Shutdown;
                }
                Some(Command::UpdateToken(t)) => config.access_token = t,
            },
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > idle_timeout {
                    return SessionEnd::Lost(format!("no traffic for {idle_timeout:?}"));
                }
                // Keep the connection provably alive; the pong counts as traffic.
                if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await {
                    return SessionEnd::Lost(format!("ping failed: {e}"));
                }
            }
        }
    }
}
