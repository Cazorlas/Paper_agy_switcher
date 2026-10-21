// The plan name and the exact "Plan until" date come from what Claude keeps
// locally (account.json) plus the profile endpoint's subscription status.

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{TimeZone, Utc};
use agy_switch::claude_api::{Endpoints, ProfileStatus, fetch_profile};
use agy_switch::claude_usage::{next_renewal, plan_label};
use serde_json::json;

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, 0).unwrap().timestamp()
}

// ── pure functions ───────────────────────────────────────

/// P1: the renewal keeps the creation day and time of day, seconds only.
#[test]
fn next_renewal_is_the_next_monthly_creation_day() {
    let now = at(2026, 10, 8, 0, 0);
    assert_eq!(
        next_renewal("2025-07-20T06:35:00.366209Z", now),
        Some(at(2026, 10, 20, 6, 35))
    );
}

/// P2: a day that a shorter month lacks falls back to that month's last day.
#[test]
fn next_renewal_clamps_to_the_last_day_of_short_months() {
    assert_eq!(
        next_renewal("2025-01-31T10:00:00Z", at(2026, 2, 10, 0, 0)),
        Some(at(2026, 2, 28, 10, 0))
    );
    assert_eq!(
        next_renewal("2025-01-31T10:00:00Z", at(2026, 3, 1, 0, 0)),
        Some(at(2026, 3, 31, 10, 0))
    );
}

/// P3: at the renewal instant itself the next renewal is a month later.
#[test]
fn next_renewal_is_strictly_after_now() {
    assert_eq!(
        next_renewal("2025-07-20T06:35:00Z", at(2026, 10, 20, 6, 35)),
        Some(at(2026, 11, 20, 6, 35))
    );
}

/// P4
#[test]
fn next_renewal_of_garbage_is_none() {
    assert_eq!(next_renewal("not a date", at(2026, 10, 8, 0, 0)), None);
}

/// P5
#[test]
fn plan_label_names_the_plan() {
    assert_eq!(plan_label(Some("default_claude_max_5x"), None, None), "max5x");
    assert_eq!(plan_label(Some("default_claude_max_20x"), None, None), "max20x");
    assert_eq!(plan_label(None, Some("claude_pro"), None), "pro");
    assert_eq!(plan_label(None, None, Some("team")), "team");
    assert_eq!(plan_label(None, None, None), "--");
}

// ── mock server ──────────────────────────────────────────

#[derive(Default)]
struct Calls {
    usage_tokens: Vec<String>,
    /// (bearer token, anthropic-beta header) of every profile request.
    profile_requests: Vec<(String, String)>,
}

#[derive(Clone)]
struct MockState {
    profile_fails: bool,
    calls: Arc<Mutex<Calls>>,
}

struct Mock {
    base: String,
    calls: Arc<Mutex<Calls>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Mock {
    async fn start(profile_fails: bool) -> Self {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let app = Router::new()
            .route("/api/oauth/usage", get(usage_handler))
            .route("/api/oauth/profile", get(profile_handler))
            .route("/v1/oauth/token", post(|| async { StatusCode::UNAUTHORIZED }))
            .with_state(MockState { profile_fails, calls: calls.clone() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        Self { base, calls, shutdown: Some(shutdown) }
    }

    fn profile_requests(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().profile_requests.clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned()
}

async fn usage_handler(State(state): State<MockState>, headers: HeaderMap) -> Response {
    let token = bearer(&headers);
    state.calls.lock().unwrap().usage_tokens.push(token.clone());
    if token != "tokA" && token != "tokB" {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(json!({
        "five_hour": {"utilization": 30.0, "resets_at": "2099-01-01T00:00:00Z"},
        "seven_day": {"utilization": 5.0, "resets_at": "2099-01-05T00:00:00Z"}
    }))
    .into_response()
}

async fn profile_handler(State(state): State<MockState>, headers: HeaderMap) -> Response {
    let token = bearer(&headers);
    let beta = headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    state.calls.lock().unwrap().profile_requests.push((token.clone(), beta));
    if state.profile_fails {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let status = if token == "tokB" { "canceled" } else { "active" };
    axum::Json(json!({
        "account": {"uuid": "U1", "email": "a@x.com"},
        "organization": {
            "uuid": "O1",
            "organization_type": "claude_max",
            "rate_limit_tier": "default_claude_max_5x",
            "subscription_status": status,
            "subscription_created_at": "2025-07-20T06:35:00.366209Z"
        }
    }))
    .into_response()
}

// ── P6: fetch_profile ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_profile_reads_the_organization_status() {
    let mock = Mock::start(false).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let endpoints = Endpoints {
        api_base: mock.base.clone(),
        token_url: format!("{}/v1/oauth/token", mock.base),
    };

    let status = fetch_profile(&client, &endpoints, "tokB")
        .await
        .unwrap_or_else(|e| panic!("fetch_profile failed: {e:?}"));

    assert_eq!(
        status,
        ProfileStatus {
            subscription_status: Some("canceled".to_owned()),
            subscription_created_at: Some("2025-07-20T06:35:00.366209Z".to_owned()),
            rate_limit_tier: Some("default_claude_max_5x".to_owned()),
            organization_type: Some("claude_max".to_owned()),
        }
    );
    let requests = mock.profile_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "tokB", "Bearer token");
    assert!(!requests[0].1.is_empty(), "anthropic-beta header");
}
