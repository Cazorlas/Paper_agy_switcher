//! Saved Antigravity accounts and quota acquisition. Fetches never write live credentials.
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use crate::{agy_api, agy_store, auth, cache, config, profile, usage};
use agy_store::{AgyStore, identity_of};

pub fn store() -> Result<AgyStore> {
    Ok(AgyStore::new(auth::profiles_dir()?, agy_store::backend_from_env()?))
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub alias: String,
    pub email: Option<String>,
}

pub fn profiles() -> Result<Vec<Profile>> {
    let store = store()?;
    Ok(profile::list_profiles()?.into_iter().filter_map(|alias| {
        let identity = identity_of(&store.profile_blob(&alias).ok()?).ok()?;
        Some(Profile { alias, email: identity.email })
    }).collect())
}

pub async fn fetch_alias(alias: &str, force: bool) -> Result<usage::UsageInfo> {
    let store = store()?;
    let mut blob = store.profile_blob(alias)?;
    let original_profile = blob.clone();
    if store.active_alias()?.as_deref() == Some(alias) {
        if let Some(live) = store.read_live()? {
            if agy_store::access_token_valid(&live, Utc::now())? { blob = live; }
        }
    }
    let client = crate::auth::build_http_client()?;
    let endpoints = agy_api::Endpoints::from_env();
    let expired = !agy_store::access_token_valid(&blob, Utc::now())?;
    if expired {
        let value: Value = serde_json::from_slice(&blob).context("invalid saved credential")?;
        let refresh = value["token"]["refresh_token"].as_str().context("missing refresh token")?;
        let response = agy_api::refresh_access_token(&client, &endpoints, refresh).await
            .map_err(|error| match error {
                agy_api::ApiError::InvalidGrant => anyhow::anyhow!(
                    "sign-in expired; sign in again with agy, then run `paper-agy-switch login {alias}`"),
                other => anyhow::anyhow!(other),
            })?;
        blob = agy_store::apply_refresh(&blob, &response, Utc::now())?;
        // Serialize profile writes against switches and login.
        let _lock = profile::lock_live_auth()?;
        if store.profile_blob(alias)? != original_profile {
            anyhow::bail!("profile changed while refreshing; retry the command");
        }
        store.store_profile_blob(alias, &blob)?;
    } else if !force {
        if let Some(cached) = cache::get(alias) { return Ok(cached); }
    }
    let value: Value = serde_json::from_slice(&blob).context("invalid saved credential")?;
    let token = value["token"]["access_token"].as_str().context("missing access token")?;
    let quota = agy_api::fetch_quota(&client, &endpoints, token).await?;
    let mut result = agy_api::usage_info(&quota);
    result.fetched_at = Some(auth::now_unix_secs());
    cache::put(alias, &result);
    Ok(result)
}

pub fn scoring_usage(mut value: usage::UsageInfo) -> usage::UsageInfo {
    if config::get().use_cfg.pool == config::ScoringPool::Gemini {
        let pool = usage::gemini_pool(&value).cloned();
        value.primary = pool.as_ref().and_then(|p| p.primary.clone());
        value.secondary = pool.and_then(|p| p.secondary.clone());
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_lookup_skips_invalid_unreadable_and_unparsable_profiles() {
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let dir = root.path().join("profiles");
        for alias in ["a-missing", "b-invalid", "c invalid", "valid"] {
            std::fs::create_dir_all(dir.join(alias)).unwrap();
        }
        std::fs::write(dir.join("b-invalid/credentials.json"), b"invalid").unwrap();
        let saved = br#"{"token":{},"id_token":"eyJhbGciOiJub25lIn0.eyJzdWIiOiJBIn0."}"#;
        std::fs::write(dir.join("c invalid/credentials.json"), saved).unwrap();
        std::fs::write(dir.join("valid/credentials.json"), saved).unwrap();
        let live_path = root.path().join("live.json");
        std::fs::write(&live_path, saved).unwrap();
        let store = AgyStore::new(dir, Box::new(agy_store::FileCredential { path: live_path }));
        assert_eq!(store.active_alias().unwrap().as_deref(), Some("valid"));
    }
}

pub fn rank(fetched: Vec<(String, usage::UsageInfo)>) -> Vec<(usage::Candidate, usage::UsageInfo, f64)> {
    let cfg = config::get();
    let now = auth::now_unix_secs();
    let size = fetched.len();
    let mut ranked: Vec<_> = fetched.into_iter().map(|(alias, value)| {
        let value = scoring_usage(value);
        let last_used = cache::get_last_used(&alias);
        let mut candidate = usage::Candidate::from_usage(alias, &value, false, false, last_used, now);
        candidate.pool_size = size;
        candidate.team_priority = cfg.use_cfg.team_priority;
        (candidate, value, 0.0)
    }).collect();
    let exhausted = ranked.iter().filter(|(c, _, _)| c.effective_used_5h() >= 100.0).count();
    for (candidate, _, score) in &mut ranked {
        candidate.pool_exhausted = exhausted;
        *score = usage::score_unified(candidate, cfg.use_cfg.safety_margin_7d);
    }
    ranked.sort_by(|a, b| {
        usage::is_candidate_eligible(&b.0, cfg.use_cfg.safety_margin_7d)
            .cmp(&usage::is_candidate_eligible(&a.0, cfg.use_cfg.safety_margin_7d))
            .then(b.2.total_cmp(&a.2))
            .then(a.0.last_used.cmp(&b.0.last_used))
            .then(a.0.alias.cmp(&b.0.alias))
    });
    ranked
}

pub async fn rank_pool(force: bool) -> Result<Vec<(usage::Candidate, usage::UsageInfo, f64)>> {
    let mut fetched = Vec::new();
    for p in profiles()? {
        if let Ok(value) = fetch_alias(&p.alias, force).await { fetched.push((p.alias, value)); }
    }
    Ok(rank(fetched))
}
