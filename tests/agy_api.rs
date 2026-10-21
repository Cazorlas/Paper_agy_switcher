mod mock;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use agy_switch::agy_api::*;
use agy_switch::usage::WindowUsage;
use axum::http::StatusCode;
use mock::{MockResponse, agy::{AgyServer, LOAD, MODELS, SUMMARY}};
use serde_json::{Value, json};

const LOAD_PROD: &str = include_str!("fixtures/cloudcode/loadCodeAssist.prod.json");
const MODELS_PROD: &str = include_str!("fixtures/cloudcode/fetchAvailableModels.prod.json");
const SUMMARY_PROD: &str = include_str!("fixtures/cloudcode/retrieveUserQuotaSummary.prod.json");
const SUMMARY_DAILY: &str = include_str!("fixtures/cloudcode/retrieveUserQuotaSummary.daily.json");
const USER_AGENT_ENV: &str = "PAPER_AGY_SWITCH_USER_AGENT";
static ENV_LOCK: Mutex<()> = Mutex::new(());

// Only this test process changes these variables; all HTTP/env tests hold ENV_LOCK.
struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvGuard {
    fn set(values: &[(&'static str, Option<&str>)]) -> Self {
        let saved = values.iter().map(|(key, _)| (*key, std::env::var_os(key))).collect();
        for (key, value) in values {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn fixture(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn ok(body: Value) -> MockResponse {
    MockResponse::json(StatusCode::OK, body)
}

fn scripts() -> Vec<(&'static str, Vec<MockResponse>)> {
    vec![
        (LOAD, vec![ok(fixture(LOAD_PROD))]),
        (MODELS, vec![ok(fixture(MODELS_PROD))]),
        (SUMMARY, vec![ok(fixture(SUMMARY_PROD))]),
    ]
}

async fn server_with(path: &'static str, status: StatusCode, body: Value) -> AgyServer {
    let mut replies = scripts();
    replies.iter_mut().find(|(route, _)| *route == path).unwrap().1 =
        vec![MockResponse::json(status, body)];
    AgyServer::start(replies).await
}

fn endpoints(servers: &[&AgyServer]) -> Endpoints {
    Endpoints {
        source: QuotaSource::Ide,
        token_url: format!("{}/token", servers[0].base),
        bases: servers.iter().map(|server| server.base.clone()).collect(),
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap()
}

fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 0.000001, "{actual} != {expected}");
}

fn timestamp(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp()
}

fn window(window: &Option<WindowUsage>, used: f64, minutes: i64, reset: &str) {
    let window = window.as_ref().expect("usage window");
    near(window.used_percent.unwrap(), used);
    assert_eq!(window.window_minutes, Some(minutes));
    assert_eq!(window.resets_at, Some(timestamp(reset)));
}

fn quota(summary: &str) -> AgyQuota {
    AgyQuota {
        code_assist: parse_load_code_assist(&fixture(LOAD_PROD)),
        buckets: parse_summary(&fixture(summary)),
        models: parse_models(&fixture(MODELS_PROD)),
        base_used: PROD_BASE.into(),
    }
}

#[test]
fn agy_api_01_paid_tier_wins() {
    assert_eq!(parse_load_code_assist(&fixture(LOAD_PROD)), CodeAssist {
        project: Some("aicode-consumers".into()),
        tier_id: Some("g1-pro-tier".into()),
        tier_name: Some("Google AI Pro".into()),
    });
}

#[test]
fn agy_api_02_object_project_and_current_tier() {
    assert_eq!(parse_load_code_assist(&json!({
        "cloudaicompanionProject": {"id":"p1"},
        "currentTier": {"id":"free-tier","name":"Free"}
    })), CodeAssist {
        project: Some("p1".into()), tier_id: Some("free-tier".into()), tier_name: Some("Free".into()),
    });
}

#[test]
fn agy_api_03_summary_preserves_all_buckets_and_precision() {
    let input = fixture(SUMMARY_PROD);
    let buckets = parse_summary(&input);
    assert_eq!(buckets.len(), 4);
    for group in input["groups"].as_array().unwrap() {
        for raw in group["buckets"].as_array().unwrap() {
            let bucket = buckets.iter().find(|b| b.bucket_id == raw["bucketId"].as_str().unwrap()).unwrap();
            assert_eq!(bucket.group, group["displayName"].as_str().unwrap());
            assert_eq!(bucket.window, raw["window"].as_str().unwrap());
            near(bucket.remaining_percent, raw["remainingFraction"].as_f64().unwrap() * 100.0);
            assert_eq!(bucket.resets_at, Some(timestamp(raw["resetTime"].as_str().unwrap())));
            assert_eq!(bucket.description.as_deref(), raw["description"].as_str());
        }
    }
    let third_party = buckets.iter().find(|b| b.bucket_id == "3p-5h").unwrap();
    assert_eq!(third_party.remaining_percent, 0.0);
    assert_eq!(third_party.group, "Claude and GPT models");
    assert_eq!(third_party.window, "5h");
    near(buckets.iter().find(|b| b.bucket_id == "gemini-weekly").unwrap().remaining_percent, 98.21287);
}

#[test]
fn agy_api_04_models_exhaustion_filter_and_sort() {
    let raw = fixture(MODELS_PROD);
    let models = parse_models(&raw);
    let mut expected_ids: Vec<_> = raw["models"].as_object().unwrap().keys()
        .filter(|id| ["gemini", "claude", "gpt"].iter().any(|part| id.contains(part)))
        .map(String::as_str).collect();
    expected_ids.sort_unstable();
    assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), expected_ids);
    assert!(models.iter().all(|m| !m.id.starts_with("chat_") && !m.id.starts_with("tab_")));
    let claude = models.iter().find(|m| m.id == "claude-opus-4-6-thinking").unwrap();
    assert!(raw["models"][&claude.id]["quotaInfo"].get("remainingFraction").is_none());
    assert_eq!(claude.remaining_percent, 0.0);
    assert_eq!(claude.resets_at, Some(timestamp(raw["models"][&claude.id]["quotaInfo"]["resetTime"].as_str().unwrap())));
    assert_eq!(claude.display_name.as_deref(), raw["models"][&claude.id]["displayName"].as_str());
    near(models.iter().find(|m| m.id == "gemini-3.1-pro-high").unwrap().remaining_percent, 100.0);
}

#[test]
fn agy_api_05_usage_info_with_gemini_pool() {
    let usage = usage_info(&quota(SUMMARY_PROD));
    window(&usage.primary, 100.0, 300, "2026-10-21T13:23:41Z");
    window(&usage.secondary, 74.79676, 10080, "2026-10-27T02:42:51Z");
    assert_eq!(usage.plan_type.as_deref(), Some("g1-pro-tier"));
    assert_eq!(usage.additional_limits.len(), 1);
    let gemini = &usage.additional_limits[0];
    assert_eq!(gemini.limit_name.as_deref(), Some("Gemini"));
    window(&gemini.primary, 0.0, 300, "2026-10-21T16:27:22Z");
    window(&gemini.secondary, 1.78713, 10080, "2026-10-22T02:37:40Z");
}

#[test]
fn agy_api_06_usage_info_with_only_third_party_pool() {
    let usage = usage_info(&quota(SUMMARY_DAILY));
    window(&usage.primary, 100.0, 300, "2026-10-21T13:23:41Z");
    window(&usage.secondary, 74.79676, 10080, "2026-10-27T02:42:51Z");
    assert!(usage.additional_limits.is_empty());
}

#[tokio::test]
async fn agy_api_07_refresh_posts_exact_form_and_preserves_response() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let reply = json!({"access_token":"new-access","expires_in":3600,"token_type":"Bearer", "refresh_token":"rotated", "id_token":"dummy-id"});
    let server = AgyServer::start(vec![("/token", vec![ok(reply.clone())])]).await;
    assert_eq!(refresh_access_token(&client(), &endpoints(&[&server]), "rt +/&=").await.unwrap(), reply);
    let calls = server.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].path, "/token");
    assert_eq!(calls[0].headers["content-type"], "application/x-www-form-urlencoded");
    // Let axum decode the form, so escaping and duplicate fields are checked too.
    use axum::extract::FromRequest;
    let request = axum::http::Request::builder()
        .method("POST")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(calls[0].body.clone())).unwrap();
    let axum::Form(fields) = axum::Form::<Vec<(String, String)>>::from_request(request, &()).await.unwrap();
    assert_eq!(fields.len(), 4);
    assert_eq!(fields.into_iter().collect::<BTreeMap<_, _>>(), BTreeMap::from([
        ("client_id".into(), "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com".into()),
        ("client_secret".into(), CLIENT_SECRET.into()),
        ("refresh_token".into(), "rt +/&=".into()),
        ("grant_type".into(), "refresh_token".into()),
    ]));
}

#[tokio::test]
async fn agy_api_08_invalid_grant_is_revoked_login() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let server = AgyServer::start(vec![("/token", vec![MockResponse::json(StatusCode::BAD_REQUEST, json!({"error":"invalid_grant"}))])]).await;
    assert!(matches!(refresh_access_token(&client(), &endpoints(&[&server]), "rt").await, Err(ApiError::InvalidGrant)));
}

#[tokio::test]
async fn agy_api_09_happy_path_headers_metadata_and_project() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvGuard::set(&[(USER_AGENT_ENV, None)]);
    let server = AgyServer::start(scripts()).await;
    let mut ep = endpoints(&[&server]);
    ep.source = QuotaSource::Cli;
    let result = fetch_quota(&client(), &ep, "test-access").await.unwrap();
    assert_eq!(result.base_used, server.base);
    assert_eq!(result.code_assist, parse_load_code_assist(&fixture(LOAD_PROD)));
    assert_eq!(result.models, parse_models(&fixture(MODELS_PROD)));
    assert_eq!(result.buckets, parse_summary(&fixture(SUMMARY_PROD)));
    let calls = server.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].path, LOAD);
    assert_eq!(serde_json::from_slice::<Value>(&calls[0].body).unwrap(), json!({
        "metadata": {"ideType":"ANTIGRAVITY"}
    }));
    let mut paths: Vec<_> = calls[1..].iter().map(|call| call.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec![MODELS, SUMMARY]);
    for call in &calls {
        assert_eq!(call.headers["authorization"], "Bearer test-access");
        assert_eq!(call.headers["user-agent"], cli_user_agent());
        assert_eq!(call.headers["content-type"], "application/json");
    }
    for call in &calls[1..] {
        assert_eq!(serde_json::from_slice::<Value>(&call.body).unwrap(), json!({"project":"aicode-consumers"}));
    }
}

#[tokio::test]
async fn agy_api_10_service_unavailable_falls_back() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = server_with(LOAD, StatusCode::SERVICE_UNAVAILABLE, json!({})).await;
    let b = AgyServer::start(scripts()).await;
    let result = fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await.unwrap();
    assert_eq!(result.base_used, b.base);
    assert_eq!(a.calls().len(), 1);
    assert_eq!(b.calls().len(), 3);
}

#[tokio::test]
async fn agy_api_11_unauthorized_does_not_fall_back() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = server_with(LOAD, StatusCode::UNAUTHORIZED, json!({})).await;
    let b = AgyServer::start(scripts()).await;
    assert!(matches!(fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await, Err(ApiError::Unauthorized)));
    assert!(b.calls().is_empty());
}

#[tokio::test]
async fn agy_api_12_forbidden_preserves_validation_url_without_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = server_with(MODELS, StatusCode::FORBIDDEN, json!({"error":{"message":"Verify your account","details":[{"metadata":{"validation_url":"https://v.example"}}]}})).await;
    let b = AgyServer::start(scripts()).await;
    match fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await {
        Err(ApiError::Forbidden { message, validation_url }) => {
            assert!(message.contains("Verify"));
            assert_eq!(validation_url.as_deref(), Some("https://v.example"));
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    assert!(b.calls().is_empty());
}

#[tokio::test]
async fn agy_api_13_rate_limit_on_any_call_does_not_fall_back() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for path in [LOAD, MODELS, SUMMARY] {
        let a = server_with(path, StatusCode::TOO_MANY_REQUESTS, json!({})).await;
        let b = AgyServer::start(scripts()).await;
        assert!(matches!(fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await, Err(ApiError::RateLimited)), "route {path}");
        assert!(b.calls().is_empty(), "route {path}");
    }
}

#[tokio::test]
async fn agy_api_14_summary_server_failures_preserve_models() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = server_with(SUMMARY, StatusCode::INTERNAL_SERVER_ERROR, json!({})).await;
    let b = server_with(SUMMARY, StatusCode::INTERNAL_SERVER_ERROR, json!({})).await;
    let result = fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await.unwrap();
    assert!(result.buckets.is_empty());
    assert_eq!(result.models, parse_models(&fixture(MODELS_PROD)));
}

#[test]
fn agy_api_15_endpoints_env_override_and_defaults() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvGuard::set(&[(CLOUDCODE_URL_ENV, Some("http://x")), (TOKEN_URL_ENV, Some("http://token"))]);
    let ep = Endpoints::from_env();
    assert_eq!(ep.bases, vec!["http://x"]);
    assert_eq!(ep.token_url, "http://token");
    let _defaults = EnvGuard::set(&[(CLOUDCODE_URL_ENV, None), (TOKEN_URL_ENV, None)]);
    let ep = Endpoints::from_env();
    assert_eq!(ep.bases, vec!["https://daily-cloudcode-pa.googleapis.com"]);
    assert_eq!(ep.token_url, "https://oauth2.googleapis.com/token");
    assert_eq!(ep.bases, vec![DAILY_BASE]);
    assert_eq!(ep.token_url, TOKEN_URL);
}

#[tokio::test]
async fn agy_api_16_missing_project_sends_empty_body_and_user_agent_override() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvGuard::set(&[(USER_AGENT_ENV, Some("test-antigravity/2"))]);
    let server = server_with(LOAD, StatusCode::OK, json!({})).await;
    fetch_quota(&client(), &endpoints(&[&server]), "tok").await.unwrap();
    let calls = server.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(call.headers["user-agent"], "test-antigravity/2");
    }
    for call in &calls[1..] {
        assert_eq!(serde_json::from_slice::<Value>(&call.body).unwrap(), json!({}));
    }
}

#[tokio::test]
async fn agy_api_17_not_found_falls_back() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = server_with(LOAD, StatusCode::NOT_FOUND, json!({})).await;
    let b = AgyServer::start(scripts()).await;
    let result = fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await.unwrap();
    assert_eq!(result.base_used, b.base);
}

#[tokio::test]
async fn agy_api_18_network_error_falls_back() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let b = AgyServer::start(scripts()).await;
    let ep = Endpoints { source: QuotaSource::Ide, token_url: format!("{}/token", b.base), bases: vec![unavailable, b.base.clone()] };
    let result = fetch_quota(&client(), &ep, "tok").await.unwrap();
    assert_eq!(result.base_used, b.base);
}

#[tokio::test]
async fn agy_api_19_ide_preserves_protocol_and_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvGuard::set(&[(USER_AGENT_ENV, None), (CLOUDCODE_URL_ENV, None)]);
    assert_eq!(Endpoints::for_source(QuotaSource::Ide).bases, vec![PROD_BASE, DAILY_BASE]);
    let a = server_with(LOAD, StatusCode::SERVICE_UNAVAILABLE, json!({})).await;
    let b = AgyServer::start(scripts()).await;
    let result = fetch_quota(&client(), &endpoints(&[&a, &b]), "tok").await.unwrap();
    assert_eq!(result.base_used, b.base);
    assert_eq!(a.calls().len(), 1);
    assert_eq!(b.calls().len(), 3);
    for call in a.calls().iter().chain(b.calls().iter()) {
        assert_eq!(call.headers["user-agent"], "antigravity/1.20.5 windows/amd64");
    }
    assert_eq!(serde_json::from_slice::<Value>(&b.calls()[0].body).unwrap(), json!({
        "metadata": {"ideName":"antigravity","ideType":"ANTIGRAVITY","ideVersion":"1.20.5",
            "pluginVersion":env!("CARGO_PKG_VERSION"),"platform":"WINDOWS_AMD64","updateChannel":"stable","pluginType":"GEMINI"},
        "mode":"FULL_ELIGIBILITY_CHECK"
    }));
}

#[tokio::test]
async fn agy_api_20_cli_never_uses_second_base() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for route in [LOAD, MODELS, SUMMARY] {
        let a = server_with(route, StatusCode::SERVICE_UNAVAILABLE, json!({})).await;
        let b = AgyServer::start(scripts()).await;
        let mut ep = endpoints(&[&a, &b]);
        ep.source = QuotaSource::Cli;
        let result = fetch_quota(&client(), &ep, "tok").await;
        if route == SUMMARY { assert!(result.unwrap().buckets.is_empty()); }
        else { assert!(matches!(result, Err(ApiError::Http { status: 503, .. }))); }
        assert!(b.calls().is_empty());
    }
}

#[tokio::test]
async fn agy_api_21_cli_user_agent_override_wins() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = EnvGuard::set(&[(USER_AGENT_ENV, Some("custom/7"))]);
    let server = AgyServer::start(scripts()).await;
    let mut ep = endpoints(&[&server]);
    ep.source = QuotaSource::Cli;
    fetch_quota(&client(), &ep, "tok").await.unwrap();
    assert!(server.calls().iter().all(|call| call.headers["user-agent"] == "custom/7"));
    let _env = EnvGuard::set(&[(CLOUDCODE_URL_ENV, Some("http://override"))]);
    for source in [QuotaSource::Cli, QuotaSource::Ide] {
        assert_eq!(Endpoints::for_source(source).bases, vec!["http://override"]);
    }
}

#[test]
fn agy_api_22_version_detection_falls_back_when_agy_is_missing() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let empty = tempfile::tempdir().unwrap();
    let _env = EnvGuard::set(&[("PATH", Some(empty.path().to_str().unwrap()))]);
    assert_eq!(detect_agy_version(), "1.4.4");
}

#[test]
fn summary_disabled_is_read_and_defaults_to_false() {
    let q = quota(include_str!("fixtures/cloudcode/retrieveUserQuotaSummary.disabled.json"));
    assert!(q.buckets[0].disabled);
    assert!(!q.buckets[1].disabled);
    near(q.buckets[0].remaining_percent, 46.8768);
    let usage = usage_info(&q);
    assert!(usage.primary.as_ref().unwrap().disabled);
    assert!(!usage.secondary.as_ref().unwrap().disabled);
    near(usage.primary.as_ref().unwrap().used_percent.unwrap(), 53.1232);
    let legacy: WindowUsage = serde_json::from_value(json!({"used_percent":53.1232})).unwrap();
    assert!(!legacy.disabled);
}

#[test]
fn usage_info_copies_disabled_for_every_window() {
    let mut q = quota(SUMMARY_PROD);
    for bucket in &mut q.buckets { bucket.disabled = true; }
    let usage = usage_info(&q);
    let gemini = &usage.additional_limits[0];
    for window in [&usage.primary, &usage.secondary, &gemini.primary, &gemini.secondary] {
        assert!(window.as_ref().unwrap().disabled);
    }
}
