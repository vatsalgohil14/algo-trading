//! The token keeper against a mock of Dhan's servers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use chrono::{FixedOffset, TimeDelta, Utc};
use dhan::token::{KeeperConfig, TokenKeeper, TokenStatus};
use dhan::{DhanConfig, ErrorKind, Secret};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const PIN: &str = "654321";

fn temp_cache(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dhan-token-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("secrets/token.json")
}

fn config(server: &MockServer, cache: &Path) -> KeeperConfig {
    let client = DhanConfig::new("1000000003", Secret::new("unused"))
        .with_api_base(server.uri())
        .with_auth_base(server.uri())
        .with_request_timeout(Duration::from_secs(2));
    let mut c = KeeperConfig::new(client, Secret::new(PIN), Secret::new("JBSWY3DPEHPK3PXP"));
    c.cache_path = cache.to_path_buf();
    c.poll_every = Duration::from_millis(20);
    c.first_retry_after = Duration::from_millis(20);
    c.min_code_seconds = 0;
    c
}

/// Expiry text as Dhan sends it, `hours` from now.
fn expiry(hours: i64) -> String {
    let ist = FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap();
    (Utc::now() + TimeDelta::hours(hours))
        .with_timezone(&ist)
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string()
}

fn issued(token: &str, hours: i64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "dhanClientId": "1000000003",
        "dhanClientName": "TEST",
        "accessToken": token,
        "expiryTime": expiry(hours),
    }))
}

fn write_cache(cache: &Path, client: &str, token: &str, expires_in_hours: i64) {
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let now = Utc::now();
    let text = json!({
        "client_id": client,
        "access_token": token,
        "expires_at": (now + TimeDelta::hours(expires_in_hours)).timestamp(),
        "generated_at": (now - TimeDelta::hours(1)).timestamp(),
    });
    std::fs::write(cache, text.to_string()).unwrap();
}

async fn profile_accepts(server: &MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/profile"))
        .and(header("access-token", token))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "dhanClientId": "1000000003", "tokenValidity": "30/03/2026 15:37"
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn issues_a_token_and_saves_it_privately() {
    let server = MockServer::start().await;
    let cache = temp_cache("issues");
    Mock::given(method("POST"))
        .and(path("/app/generateAccessToken"))
        .and(query_param("dhanClientId", "1000000003"))
        .and(query_param("pin", PIN))
        .respond_with(issued("fresh-token", 48))
        .expect(1)
        .mount(&server)
        .await;

    let keeper = TokenKeeper::new(config(&server, &cache)).unwrap();
    let (client, token) = keeper.connect().await.unwrap();
    assert_eq!(client.access_token().expose(), "fresh-token");
    assert!(token.expires > Utc::now() + TimeDelta::hours(47));
    assert!(!format!("{token:?}").contains("fresh-token"));
    assert!(!format!("{keeper:?}").contains(PIN));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&cache), 0o600);
        assert_eq!(mode(cache.parent().unwrap()), 0o700);
    }
    assert!(
        std::fs::read_to_string(&cache)
            .unwrap()
            .contains("fresh-token")
    );
}

#[tokio::test]
async fn reuses_a_saved_token_that_is_still_good() {
    let server = MockServer::start().await;
    let cache = temp_cache("reuse");
    write_cache(&cache, "1000000003", "saved-token", 48);
    profile_accepts(&server, "saved-token").await;
    Mock::given(path("/app/generateAccessToken"))
        .respond_with(issued("x", 48))
        .expect(0)
        .mount(&server)
        .await;

    let keeper = TokenKeeper::new(config(&server, &cache)).unwrap();
    let (client, _) = keeper.connect().await.unwrap();
    assert_eq!(client.access_token().expose(), "saved-token");
}

#[tokio::test]
async fn replaces_an_expired_or_foreign_saved_token() {
    for (name, client_id, hours) in [
        ("expired", "1000000003", -1),
        ("foreign", "someone-else", 48),
    ] {
        let server = MockServer::start().await;
        let cache = temp_cache(name);
        write_cache(&cache, client_id, "old-token", hours);
        Mock::given(path("/app/generateAccessToken"))
            .respond_with(issued("new-token", 48))
            .expect(1)
            .mount(&server)
            .await;
        let keeper = TokenKeeper::new(config(&server, &cache)).unwrap();
        let (client, _) = keeper.connect().await.unwrap();
        assert_eq!(client.access_token().expose(), "new-token", "{name}");
    }
}

#[tokio::test]
async fn replaces_a_saved_token_dhan_no_longer_accepts() {
    let server = MockServer::start().await;
    let cache = temp_cache("revoked");
    write_cache(&cache, "1000000003", "revoked-token", 48);
    Mock::given(path("/profile"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errorType": "Invalid_Authentication", "errorCode": "DH-901", "errorMessage": "bad token"
        })))
        .mount(&server)
        .await;
    Mock::given(path("/app/generateAccessToken"))
        .respond_with(issued("new-token", 48))
        .expect(1)
        .mount(&server)
        .await;
    let keeper = TokenKeeper::new(config(&server, &cache)).unwrap();
    let (client, _) = keeper.connect().await.unwrap();
    assert_eq!(client.access_token().expose(), "new-token");
}

#[tokio::test]
async fn a_network_problem_does_not_burn_a_good_saved_token() {
    let server = MockServer::start().await;
    let cache = temp_cache("flaky");
    write_cache(&cache, "1000000003", "saved-token", 48);
    Mock::given(path("/profile"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(path("/app/generateAccessToken"))
        .respond_with(issued("x", 48))
        .expect(0)
        .mount(&server)
        .await;
    let keeper = TokenKeeper::new(config(&server, &cache)).unwrap();
    let (client, _) = keeper.connect().await.unwrap();
    assert_eq!(client.access_token().expose(), "saved-token");
}

/// Fails `failures` times with a server error, then issues a token.
struct FlakyThenOk {
    failures: u32,
    calls: AtomicU32,
}

impl Respond for FlakyThenOk {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.failures {
            ResponseTemplate::new(500)
        } else {
            issued("renewed-token", 48)
        }
    }
}

async fn wait_for(
    handle: &dhan::token::RefreshHandle,
    done: impl Fn(&TokenStatus) -> bool,
) -> TokenStatus {
    for _ in 0..500 {
        let status = handle.status();
        if done(&status) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("status stayed {:?}", handle.status());
}

/// A token that is already due, issued long enough ago to allow a refresh.
fn due_token_file(cache: &Path) {
    write_cache(cache, "1000000003", "old-token", 0);
}

#[tokio::test]
async fn background_refresh_retries_then_swaps_the_token() {
    let server = MockServer::start().await;
    let cache = temp_cache("refresh");
    Mock::given(path("/app/generateAccessToken"))
        .respond_with(issued("first-token", 1))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(path("/app/generateAccessToken"))
        .respond_with(FlakyThenOk {
            failures: 2,
            calls: AtomicU32::new(0),
        })
        .mount(&server)
        .await;

    due_token_file(&cache);
    let mut settings = config(&server, &cache);
    settings.poll_every = Duration::from_millis(5);
    let keeper = TokenKeeper::new(settings.clone()).unwrap();
    let (client, mut token) = keeper.connect().await.unwrap();
    assert_eq!(client.access_token().expose(), "first-token");

    // Pretend it was issued long ago and is about to run out.
    token.expires = Utc::now() + TimeDelta::minutes(1);
    let token = pretend_old(token);
    let handle = TokenKeeper::new(settings)
        .unwrap()
        .spawn_refresh(client.clone(), &token);
    wait_for(&handle, |s| matches!(s, TokenStatus::Retrying { .. })).await;
    let status = wait_for(&handle, |s| matches!(s, TokenStatus::Healthy { expires } if *expires > Utc::now() + TimeDelta::hours(40))).await;
    assert!(matches!(status, TokenStatus::Healthy { .. }));
    assert_eq!(client.access_token().expose(), "renewed-token");
    assert!(
        std::fs::read_to_string(&cache)
            .unwrap()
            .contains("renewed-token")
    );
}

/// A copy of `token` with no record of when it was issued.
fn pretend_old(token: dhan::token::ValidToken) -> dhan::token::ValidToken {
    dhan::token::ValidToken::for_testing(token.token, token.expires)
}

#[tokio::test]
async fn a_refusal_stops_at_once() {
    let server = MockServer::start().await;
    let cache = temp_cache("refused");
    Mock::given(path("/app/generateAccessToken"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errorType": "Invalid_Authentication", "errorCode": "DH-901", "errorMessage": "wrong pin"
        })))
        .expect(2) // once on connect, once from the background
        .mount(&server)
        .await;
    let settings = config(&server, &cache);

    let keeper = TokenKeeper::new(settings.clone()).unwrap();
    let error = keeper.connect().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);

    let client = dhan::DhanClient::new(
        DhanConfig::new("1000000003", Secret::new("old")).with_api_base(server.uri()),
    )
    .unwrap();
    let token =
        dhan::token::ValidToken::for_testing(Secret::new("old"), Utc::now() - TimeDelta::hours(1));
    let handle = TokenKeeper::new(settings)
        .unwrap()
        .spawn_refresh(client, &token);
    let status = wait_for(&handle, |s| matches!(s, TokenStatus::Stopped { .. })).await;
    assert!(format!("{status:?}").contains("wrong pin"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    // `expect(2)` above fails the test if it tried again.
}

#[tokio::test]
async fn errors_never_show_the_pin() {
    let cache = temp_cache("leak");
    let client =
        DhanConfig::new("1000000003", Secret::new("unused")).with_auth_base("http://127.0.0.1:1");
    let mut settings = KeeperConfig::new(client, Secret::new(PIN), Secret::new("JBSWY3DPEHPK3PXP"));
    settings.cache_path = cache;
    settings.min_code_seconds = 0;
    let error = TokenKeeper::new(settings)
        .unwrap()
        .connect()
        .await
        .unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(!text.contains(PIN), "{text}");
    assert!(!text.contains("totp="), "{text}");
}
