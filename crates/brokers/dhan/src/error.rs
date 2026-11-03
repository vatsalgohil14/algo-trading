use std::time::Duration;

use serde_json::Value;

use crate::rate_limit::ApiCategory;

/// Broad class of a failure, used to decide what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Token missing, invalid or expired (DH-901, 807-810). Get a new token.
    Auth,
    /// Not subscribed to Data APIs or no Trading API access (DH-902, 806).
    Access,
    /// Account setup problem, e.g. segment not activated (DH-903).
    Account,
    /// Throttled by Dhan or by the local limiter (DH-904, 805, HTTP 429).
    RateLimit,
    /// Bad request parameters (DH-905, 804, 811-814).
    Input,
    /// Order rejected as invalid (DH-906).
    Order,
    /// No data for the request (DH-907).
    Data,
    /// Dhan-side failure (DH-908, 800, HTTP 5xx).
    Server,
    /// Network failure between client and Dhan, or Dhan and its backend (DH-909).
    Network,
    /// A WebSocket protocol or connection failure.
    WebSocket,
    /// Response could not be decoded.
    Decode,
    /// Anything else (DH-910 and unclassified errors).
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum DhanError {
    /// Rejected locally before anything was sent.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Dhan answered with a non-success status.
    #[error("Dhan API error (HTTP {status}, code {code:?}): {message}")]
    Api {
        status: u16,
        kind: ErrorKind,
        code: Option<String>,
        error_type: Option<String>,
        message: String,
    },

    /// The HTTP exchange failed. `outcome_unknown` is true when the request
    /// may have reached Dhan (for an order: it may or may not have been placed).
    #[error("HTTP transport error (outcome unknown: {outcome_unknown}): {source}")]
    Transport {
        #[source]
        source: reqwest::Error,
        outcome_unknown: bool,
    },

    /// Waiting for the local rate limiter would take longer than allowed.
    #[error("local rate limit for {category:?} APIs: next slot in {wait:?}")]
    RateLimitedLocally {
        category: ApiCategory,
        wait: Duration,
    },

    #[error("failed to decode response from {endpoint}: {message}; body: {body}")]
    Decode {
        endpoint: String,
        message: String,
        body: String,
    },

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("instrument master error: {0}")]
    Instruments(String),

    /// The background task behind a stream handle has stopped.
    #[error("background task has stopped")]
    ChannelClosed,

    #[error("TOTP error: {0}")]
    Totp(String),
}

impl DhanError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            DhanError::InvalidRequest(_) => ErrorKind::Input,
            DhanError::Api { kind, .. } => *kind,
            DhanError::Transport { .. } => ErrorKind::Network,
            DhanError::RateLimitedLocally { .. } => ErrorKind::RateLimit,
            DhanError::Decode { .. } => ErrorKind::Decode,
            DhanError::WebSocket(_) | DhanError::ChannelClosed => ErrorKind::WebSocket,
            DhanError::Instruments(_) | DhanError::Totp(_) => ErrorKind::Other,
        }
    }

    /// True when retrying the same call later may succeed. This says nothing
    /// about whether a retry is *safe*; for orders check
    /// [`outcome_unknown`](Self::outcome_unknown) first.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind(),
            ErrorKind::RateLimit | ErrorKind::Server | ErrorKind::Network
        )
    }

    /// True when the request may have been executed by Dhan even though no
    /// usable response came back. For orders: reconcile before retrying.
    pub fn outcome_unknown(&self) -> bool {
        match self {
            DhanError::Transport {
                outcome_unknown, ..
            } => *outcome_unknown,
            // A 5xx means Dhan received the request; whether it acted on it is
            // not knowable from here.
            DhanError::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }

    pub(crate) fn transport(source: reqwest::Error) -> Self {
        // A failed connect or a request that could not be built never left
        // this process; anything else may have reached Dhan.
        let outcome_unknown = !(source.is_connect() || source.is_builder());
        DhanError::Transport {
            source,
            outcome_unknown,
        }
    }

    pub(crate) fn decode(endpoint: &str, message: impl ToString, body: &[u8]) -> Self {
        DhanError::Decode {
            endpoint: endpoint.to_owned(),
            message: message.to_string(),
            body: snippet(body),
        }
    }

    /// Builds an API error from an HTTP status and response body, accepting
    /// the several error shapes Dhan uses.
    pub(crate) fn from_response(status: u16, body: &[u8]) -> Self {
        let parsed: Option<Value> = serde_json::from_slice(body).ok();
        let (code, error_type, message) = parsed
            .as_ref()
            .map(extract_error_fields)
            .unwrap_or((None, None, None));
        let kind = classify(status, code.as_deref());
        DhanError::Api {
            status,
            kind,
            code,
            error_type,
            message: message.unwrap_or_else(|| snippet(body)),
        }
    }
}

fn extract_error_fields(v: &Value) -> (Option<String>, Option<String>, Option<String>) {
    let s = |v: Option<&Value>| -> Option<String> {
        match v? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };

    // Trading API shape: {"errorType", "errorCode", "errorMessage"}
    if v.get("errorCode").is_some() || v.get("errorMessage").is_some() {
        return (
            s(v.get("errorCode")),
            s(v.get("errorType")),
            s(v.get("errorMessage")),
        );
    }
    // Data API shape: {"status": "failed", "remarks": {"error_code", ...}}
    if let Some(r) = v.get("remarks").filter(|r| r.is_object()) {
        return (
            s(r.get("error_code")),
            s(r.get("error_type")),
            s(r.get("error_message")),
        );
    }
    // Quote API shape: {"status": "failed", "data": {"805": "message"}}
    if let Some(Value::Object(map)) = v.get("data")
        && let Some((code, msg)) = map.iter().next()
    {
        return (Some(code.clone()), None, s(Some(msg)));
    }
    (None, None, s(v.get("message")))
}

fn classify(status: u16, code: Option<&str>) -> ErrorKind {
    let by_code = code.and_then(|c| match c.trim() {
        "DH-901" | "807" | "808" | "809" | "810" => Some(ErrorKind::Auth),
        "DH-902" | "806" => Some(ErrorKind::Access),
        "DH-903" => Some(ErrorKind::Account),
        "DH-904" | "805" => Some(ErrorKind::RateLimit),
        "DH-905" | "804" | "811" | "812" | "813" | "814" => Some(ErrorKind::Input),
        "DH-906" => Some(ErrorKind::Order),
        "DH-907" => Some(ErrorKind::Data),
        "DH-908" | "800" => Some(ErrorKind::Server),
        "DH-909" => Some(ErrorKind::Network),
        "DH-910" => Some(ErrorKind::Other),
        _ => None,
    });
    by_code.unwrap_or(match status {
        401 | 403 => ErrorKind::Auth,
        429 => ErrorKind::RateLimit,
        400 | 404 | 405 | 409 | 422 => ErrorKind::Input,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Other,
    })
}

fn snippet(body: &[u8]) -> String {
    const MAX: usize = 512;
    let text = String::from_utf8_lossy(body);
    if text.len() <= MAX {
        return text.into_owned();
    }
    let mut end = MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text.get(..end).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trading_error_shape() {
        let body = br#"{"errorType":"Rate_Limit","errorCode":"DH-904","errorMessage":"Too many requests"}"#;
        let e = DhanError::from_response(429, body);
        assert_eq!(e.kind(), ErrorKind::RateLimit);
        assert!(e.is_retryable());
        assert!(!e.outcome_unknown());
    }

    #[test]
    fn data_error_shape() {
        let body = br#"{"status":"failed","remarks":{"error_code":"DH-905","error_type":"Input_Exception","error_message":"bad"},"data":""}"#;
        let e = DhanError::from_response(400, body);
        assert_eq!(e.kind(), ErrorKind::Input);
    }

    #[test]
    fn quote_error_shape() {
        let body = br#"{"status":"failed","data":{"807":"Access token is expired"}}"#;
        let e = DhanError::from_response(401, body);
        assert_eq!(e.kind(), ErrorKind::Auth);
    }

    #[test]
    fn non_json_error_falls_back_to_status() {
        let e = DhanError::from_response(503, b"<html>bad gateway</html>");
        assert_eq!(e.kind(), ErrorKind::Server);
        assert!(e.outcome_unknown());
    }
}
