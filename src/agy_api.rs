//! Google Antigravity OAuth and Cloud Code quota client.

use std::time::Duration;

use serde_json::{Value, json};

pub const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const CLIENT_SECRET: &str = "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";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const PROD_BASE: &str = "https://cloudcode-pa.googleapis.com";
pub const DAILY_BASE: &str = "https://daily-cloudcode-pa.googleapis.com";
pub const TOKEN_URL_ENV: &str = "PAPER_AGY_SWITCH_OAUTH_URL";
pub const CLOUDCODE_URL_ENV: &str = "PAPER_AGY_SWITCH_CLOUDCODE_URL";

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub token_url: String,
    pub bases: Vec<String>,
}

impl Endpoints {
    pub fn from_env() -> Self {
        Self {
            token_url: std::env::var(TOKEN_URL_ENV).unwrap_or_else(|_| TOKEN_URL.into()),
            bases: std::env::var(CLOUDCODE_URL_ENV)
                .map(|base| vec![base])
                .unwrap_or_else(|_| vec![PROD_BASE.into(), DAILY_BASE.into()]),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("login revoked")]
    InvalidGrant,
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {message}")]
    Forbidden { message: String, validation_url: Option<String> },
    #[error("rate limited")]
    RateLimited,
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("network: {0}")]
    Network(String),
    #[error("parse: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodeAssist {
    pub project: Option<String>,
    pub tier_id: Option<String>,
    pub tier_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelQuota {
    pub id: String,
    pub display_name: Option<String>,
    pub remaining_percent: f64,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaBucket {
    pub bucket_id: String,
    pub group: String,
    pub window: String,
    pub remaining_percent: f64,
    pub resets_at: Option<i64>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgyQuota {
    pub code_assist: CodeAssist,
    pub buckets: Vec<QuotaBucket>,
    pub models: Vec<ModelQuota>,
    pub base_used: String,
}

pub fn parse_load_code_assist(v: &Value) -> CodeAssist {
    let project = &v["cloudaicompanionProject"];
    let tier = v.get("paidTier").filter(|tier| tier.is_object())
        .unwrap_or(&v["currentTier"]);
    CodeAssist {
        project: project.as_str().or_else(|| project["id"].as_str()).map(str::to_owned),
        tier_id: tier["id"].as_str().map(str::to_owned),
        tier_name: tier["name"].as_str().map(str::to_owned),
    }
}

fn reset_time(v: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|time| time.timestamp())
}

pub fn parse_models(v: &Value) -> Vec<ModelQuota> {
    let mut models: Vec<_> = v["models"].as_object().into_iter().flatten()
        .filter(|(id, _)| ["gemini", "claude", "gpt"].iter().any(|part| id.contains(part)))
        .filter_map(|(id, model)| {
            let quota = &model["quotaInfo"];
            let remaining = quota["remainingFraction"].as_f64()
                .or_else(|| quota["resetTime"].as_str().map(|_| 0.0))?;
            Some(ModelQuota {
                id: id.clone(),
                display_name: model["displayName"].as_str().map(str::to_owned),
                remaining_percent: remaining * 100.0,
                resets_at: reset_time(&quota["resetTime"]),
            })
        }).collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

pub fn parse_summary(v: &Value) -> Vec<QuotaBucket> {
    v["groups"].as_array().into_iter().flatten().flat_map(|group| {
        group["buckets"].as_array().into_iter().flatten().filter_map(|bucket| {
            Some(QuotaBucket {
                bucket_id: bucket["bucketId"].as_str()?.into(),
                group: group["displayName"].as_str()?.into(),
                window: bucket["window"].as_str()?.into(),
                remaining_percent: bucket["remainingFraction"].as_f64()? * 100.0,
                resets_at: reset_time(&bucket["resetTime"]),
                description: bucket["description"].as_str().map(str::to_owned),
            })
        })
    }).collect()
}

// reqwest errors can include request URLs; never retain them or request headers.
fn network_error(error: reqwest::Error) -> ApiError {
    let message = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    ApiError::Network(message.into())
}

fn safe_text(text: &str, token: &str) -> String {
    let text = if token.is_empty() { text.to_owned() } else { text.replace(token, "[redacted]") };
    // Also cover escaped token values in a JSON error body.
    let encoded = serde_json::to_string(token).unwrap_or_default();
    let escaped = encoded.trim_matches('"');
    let text = if escaped.is_empty() { text } else { text.replace(escaped, "[redacted]") };
    text.chars().take(300).collect()
}

async fn response_json(response: reqwest::Response, token: &str) -> Result<Value, ApiError> {
    let status = response.status().as_u16();
    // These statuses need no server-supplied diagnostics.
    match status {
        401 => return Err(ApiError::Unauthorized),
        429 => return Err(ApiError::RateLimited),
        _ => {}
    }
    let bytes = response.bytes().await.map_err(network_error)?;
    let parsed = serde_json::from_slice::<Value>(&bytes);
    if status == 400 && parsed.as_ref().ok().and_then(|v| v["error"].as_str()) == Some("invalid_grant") {
        return Err(ApiError::InvalidGrant);
    }
    if status == 403 {
        let value = parsed.unwrap_or(Value::Null);
        return Err(ApiError::Forbidden {
            message: safe_text(value["error"]["message"].as_str().unwrap_or("forbidden"), token),
            validation_url: value["error"]["details"].as_array().into_iter().flatten()
                .find_map(|detail| detail["metadata"]["validation_url"].as_str())
                .map(|url| safe_text(url, token)),
        });
    }
    if !(200..300).contains(&status) {
        return Err(ApiError::Http { status, body: safe_text(&String::from_utf8_lossy(&bytes), token) });
    }
    parsed.map_err(|_| ApiError::Parse("invalid response JSON".into()))
}

pub async fn refresh_access_token(
    client: &reqwest::Client,
    ep: &Endpoints,
    refresh_token: &str,
) -> Result<serde_json::Value, ApiError> {
    let response = client.post(&ep.token_url)
        .timeout(Duration::from_secs(15))
        .form(&[
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send().await.map_err(network_error)?;
    response_json(response, refresh_token).await
}

fn can_fallback(error: &ApiError) -> bool {
    matches!(error, ApiError::Network(_) | ApiError::Http { status: 404 | 500..=599, .. })
}

async fn cloudcode(
    client: &reqwest::Client,
    base: &str,
    route: &str,
    token: &str,
    user_agent: &str,
    body: &Value,
) -> Result<Value, ApiError> {
    let response = client.post(format!("{}/v1internal:{route}", base.trim_end_matches('/')))
        .bearer_auth(token)
        .header(reqwest::header::USER_AGENT, user_agent)
        .json(body)
        .send().await.map_err(network_error)?;
    response_json(response, token).await
}

pub async fn fetch_quota(
    client: &reqwest::Client,
    ep: &Endpoints,
    access_token: &str,
) -> Result<AgyQuota, ApiError> {
    let user_agent = std::env::var("PAPER_AGY_SWITCH_USER_AGENT")
        .unwrap_or_else(|_| "antigravity/1.20.5 windows/amd64".into());
    let load_body = json!({
        "metadata": {
            "ideName": "antigravity", "ideType": "ANTIGRAVITY", "ideVersion": "1.20.5",
            "pluginVersion": env!("CARGO_PKG_VERSION"), "platform": "WINDOWS_AMD64",
            "updateChannel": "stable", "pluginType": "GEMINI"
        },
        "mode": "FULL_ELIGIBILITY_CHECK"
    });
    let mut last_error = ApiError::Network("no Cloud Code endpoints configured".into());
    for (index, base) in ep.bases.iter().enumerate() {
        let load = match cloudcode(client, base, "loadCodeAssist", access_token, &user_agent, &load_body).await {
            Ok(load) => load,
            Err(error) if can_fallback(&error) => { last_error = error; continue; }
            Err(error) => return Err(error),
        };
        let code_assist = parse_load_code_assist(&load);
        let body = code_assist.project.as_ref().map(|project| json!({"project": project}))
            .unwrap_or_else(|| json!({}));
        let models = match cloudcode(client, base, "fetchAvailableModels", access_token, &user_agent, &body).await {
            Ok(models) => parse_models(&models),
            Err(error) if can_fallback(&error) => { last_error = error; continue; }
            Err(error) => return Err(error),
        };
        let mut buckets = Vec::new();
        // Summary is optional, except a rate limit must stop all requests.
        // Retry only that route on remaining bases, keeping the working models.
        for summary_base in &ep.bases[index..] {
            match cloudcode(client, summary_base, "retrieveUserQuotaSummary", access_token, &user_agent, &body).await {
                Ok(summary) => { buckets = parse_summary(&summary); break; }
                Err(ApiError::RateLimited) => return Err(ApiError::RateLimited),
                Err(error) if can_fallback(&error) => continue,
                Err(_) => break,
            }
        }
        return Ok(AgyQuota { code_assist, buckets, models, base_used: base.clone() });
    }
    Err(last_error)
}

pub fn usage_info(q: &AgyQuota) -> crate::usage::UsageInfo {
    use crate::usage::{AdditionalRateLimit, UsageInfo, WindowUsage};

    let window = |id: &str, minutes| {
        q.buckets.iter().find(|bucket| bucket.bucket_id == id).map(|bucket| WindowUsage {
            used_percent: Some(100.0 - bucket.remaining_percent),
            resets_at: bucket.resets_at,
            window_minutes: Some(minutes),
        })
    };
    let gemini_primary = window("gemini-5h", 300);
    let gemini_secondary = window("gemini-weekly", 10080);
    let mut additional_limits = Vec::new();
    if gemini_primary.is_some() || gemini_secondary.is_some() {
        additional_limits.push(AdditionalRateLimit {
            limit_name: Some("Gemini".into()),
            primary: gemini_primary,
            secondary: gemini_secondary,
            ..Default::default()
        });
    }
    UsageInfo {
        primary: window("3p-5h", 300),
        secondary: window("3p-weekly", 10080),
        plan_type: q.code_assist.tier_id.clone(),
        additional_limits,
        ..Default::default()
    }
}
