use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{agy_api, agy_store, agy_usage, auth, output, profile};

const SCOPES: &str = "openid https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs https://www.googleapis.com/auth/aicode";

fn random_secret() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("OS random generator unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn reply(stream: &mut TcpStream, status: &str, text: &str) -> Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())?;
    Ok(())
}

fn wait_for_code(listener: TcpListener, state: &str, timeout: Duration) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline { bail!("timed out waiting for Google sign-in"); }
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let request_deadline = deadline.min(Instant::now() + Duration::from_secs(5));
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let remaining = request_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { bail!("OAuth callback request timed out"); }
            stream.set_read_timeout(Some(remaining))?;
            let mut byte = [0];
            if stream.read(&mut byte)? == 0 { bail!("incomplete OAuth callback request"); }
            request.push(byte[0]);
            if request.len() > 32 * 1024 {
                let _ = reply(&mut stream, "413 Payload Too Large", "Request too large.");
                bail!("OAuth callback exceeds 32 KiB");
            }
        }
        let request = std::str::from_utf8(&request).context("invalid callback request")?;
        let mut parts = request.lines().next().unwrap_or_default().split_whitespace();
        let method = parts.next();
        let target = parts.next().context("missing callback path")?;
        let url = reqwest::Url::parse(&format!("http://localhost{target}"))?;
        if url.path() != "/oauth-callback" {
            reply(&mut stream, "404 Not Found", "Not found.")?;
            continue;
        }
        let values: Vec<_> = url.query_pairs().collect();
        let field = |key: &str| -> Option<&str> {
            let mut matches = values.iter().filter(|(k, _)| k == key);
            let value = matches.next()?.1.as_ref();
            if matches.next().is_some() { None } else { Some(value) }
        };
        if method != Some("GET") || field("state") != Some(state) {
            let _ = reply(&mut stream, "400 Bad Request", "Sign-in failed: invalid state. Return to the terminal.");
            bail!("OAuth callback state mismatch or missing state");
        }
        if let Some(error) = field("error") {
            let _ = reply(&mut stream, "400 Bad Request", "Google sign-in failed. Return to the terminal.");
            bail!("Google sign-in failed: {error}");
        }
        let code = field("code").filter(|v| !v.is_empty()).context("OAuth callback missing code")?.to_owned();
        reply(&mut stream, "200 OK", "Sign-in complete. Return to the terminal.")?;
        return Ok(code);
    }
}

#[derive(Serialize)]
struct Token<'a> {
    access_token: &'a str,
    token_type: &'a str,
    refresh_token: &'a str,
    expiry: String,
    expires_in: i64,
}

#[derive(Serialize)]
struct Blob<'a> { token: Token<'a>, auth_method: &'a str, id_token: &'a str }

pub(crate) async fn add_cmd(alias: Option<&str>, timeout_secs: u64, json: bool) -> Result<()> {
    if let Some(alias) = alias { profile::validate_alias(alias)?; }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let redirect = format!("http://localhost:{}/oauth-callback", listener.local_addr()?.port());
    let state = random_secret()?;
    let verifier = random_secret()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", agy_api::CLIENT_ID), ("redirect_uri", &redirect),
        ("response_type", "code"), ("access_type", "offline"), ("prompt", "consent"),
        ("state", &state), ("scope", SCOPES), ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ]);
    if std::env::var("PAPER_AGY_SWITCH_NO_BROWSER").as_deref() == Ok("1") {
        println!("AUTH_URL {url}");
    } else {
        output::user_println(url.as_str());
        #[cfg(windows)]
        let result = {
            use std::os::windows::process::CommandExt;
            std::process::Command::new("cmd.exe").raw_arg(format!("/c start \"\" \"{url}\"")).spawn()
        };
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("open").arg(url.as_str()).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let result = std::process::Command::new("xdg-open").arg(url.as_str()).spawn();
        if let Err(error) = result { eprintln!("Could not open browser ({error}); open the URL above."); }
    }
    std::io::stdout().flush()?;
    let code = tokio::task::spawn_blocking(move || wait_for_code(listener, &state, Duration::from_secs(timeout_secs))).await??;
    let response = auth::build_http_client()?.post(&agy_api::Endpoints::from_env().token_url)
        .form(&[("client_id", agy_api::CLIENT_ID), ("client_secret", agy_api::CLIENT_SECRET),
            ("code", &code), ("redirect_uri", &redirect), ("grant_type", "authorization_code"), ("code_verifier", &verifier)])
        .send().await.context("Google token exchange failed")?;
    if !response.status().is_success() { bail!("Google token exchange failed (HTTP {})", response.status()); }
    let value: serde_json::Value = response.json().await.map_err(|_| anyhow::anyhow!("invalid Google token response"))?;
    let required = |key| value.get(key).and_then(serde_json::Value::as_str).filter(|v| !v.is_empty())
        .with_context(|| format!("Google token response missing {key}; sign in again with consent"));
    let access_token = required("access_token")?;
    let refresh_token = required("refresh_token")?;
    let id_token = required("id_token")?;
    let seconds = value["expires_in"].as_i64().filter(|s| *s > 0).context("invalid expires_in in Google token response")?;
    let expiry = chrono::Duration::try_seconds(seconds).and_then(|d| chrono::Utc::now().checked_add_signed(d)).context("token expiry out of range")?;
    let blob = serde_json::to_vec(&Blob { token: Token { access_token, token_type: "Bearer", refresh_token,
        expiry: expiry.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), expires_in: seconds }, auth_method: "consumer", id_token })?;
    let identity = agy_store::identity_of(&blob)?;
    let email = identity.email.as_deref().context("Google id_token missing email")?;
    let derived: String = email.split('@').next().unwrap_or("account").chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).take(64).collect();
    let derived = if derived.is_empty() || derived == "." || derived == ".." { "account" } else { &derived };
    let alias = alias.unwrap_or(derived);
    let _lock = profile::lock_live_auth()?;
    let store = agy_usage::store()?;
    if crate::profile::list_profiles()?.iter().any(|a| a == alias)
        && !agy_store::same_account(&agy_store::identity_of(&store.profile_blob(alias)?)?, &identity) {
        bail!("profile belongs to another account");
    }
    store.store_profile_blob(alias, &blob)?;
    if json { output::print_json(&serde_json::json!({"ok":true,"alias":alias,"email":email})); }
    else { println!("Added {alias} ({email}). Switch with: paper-agy-switch use {alias}"); }
    Ok(())
}
