use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct Call {
    pub path: String,
    pub token: String,
    pub body: String,
}

#[derive(Clone)]
struct MockState {
    calls: Arc<Mutex<Vec<Call>>>,
    reject_b: bool,
}

pub struct Mock {
    pub base: String,
    calls: Arc<Mutex<Vec<Call>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Mock {
    pub async fn start(reject_b: bool) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/token", post(handler))
            .route("/v1internal:loadCodeAssist", post(handler))
            .route("/v1internal:fetchAvailableModels", post(handler))
            .route("/v1internal:retrieveUserQuotaSummary", post(handler))
            .with_state(MockState { calls: calls.clone(), reject_b });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { let _ = stopped.await; })
                .await.unwrap();
        });
        Self { base, calls, shutdown: Some(shutdown) }
    }

    pub fn calls(&self) -> Vec<Call> { self.calls.lock().unwrap().clone() }
}

impl Drop for Mock {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() { let _ = shutdown.send(()); }
    }
}

async fn handler(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").to_owned();
    let body = String::from_utf8(body.to_vec()).unwrap();
    state.calls.lock().unwrap().push(Call { path: uri.path().into(), token: token.clone(), body: body.clone() });
    if uri.path() == "/token" {
        // The Google endpoint takes application/x-www-form-urlencoded, not JSON.
        let fields: std::collections::HashMap<_, _> = body.split('&')
            .filter_map(|pair| pair.split_once('=')).collect();
        if fields.get("grant_type") != Some(&"refresh_token") {
            return (StatusCode::BAD_REQUEST, axum::Json(json!({"error":"unsupported_grant_type"}))).into_response();
        }
        let account = match fields.get("refresh_token").copied() {
            Some("rtA") => "A",
            Some("rtB") if !state.reject_b => "B",
            _ => return (StatusCode::BAD_REQUEST, axum::Json(json!({"error":"invalid_grant"}))).into_response(),
        };
        return axum::Json(json!({"access_token":format!("tok{account}2"), "token_type":"Bearer", "expires_in":3600})).into_response();
    }
    let (claude, gemini, weekly) = match token.as_str() {
        "tokA" | "tokA2" | "tokAfresh" => (0.2, 0.9, 0.8),
        "tokB" | "tokB2" => (0.9, 0.1, 0.95),
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let reset = |hours| (chrono::Utc::now() + chrono::Duration::hours(hours)).to_rfc3339();
    let result = match uri.path() {
        "/v1internal:loadCodeAssist" => json!({
            "cloudaicompanionProject":"test-project",
            "paidTier":{"id":"g1-pro-tier", "name":"Google AI Pro"}
        }),
        "/v1internal:fetchAvailableModels" => json!({"models":{
            "gemini-pro":{"displayName":"Gemini Pro", "quotaInfo":{"remainingFraction":gemini,"resetTime":reset(5)}},
            "claude-sonnet":{"displayName":"Claude Sonnet", "quotaInfo":{"remainingFraction":claude,"resetTime":reset(5)}}
        }}),
        "/v1internal:retrieveUserQuotaSummary" => json!({"groups":[
            {"displayName":"Claude and GPT models", "buckets":[
                {"bucketId":"3p-5h","window":"5h","remainingFraction":claude,"resetTime":reset(5)},
                {"bucketId":"3p-weekly","window":"weekly","remainingFraction":weekly,"resetTime":reset(168)}]},
            {"displayName":"Gemini Models", "buckets":[
                {"bucketId":"gemini-5h","window":"5h","remainingFraction":gemini,"resetTime":reset(5)},
                {"bucketId":"gemini-weekly","window":"weekly","remainingFraction":weekly,"resetTime":reset(168)}]}
        ]}),
        _ => unreachable!(),
    };
    axum::Json(result).into_response()
}
