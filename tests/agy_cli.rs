//! RED contract for the Antigravity CLI. All credentials are synthetic.
#[path = "mock/agy_cli.rs"]
mod mock;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

fn blob(account: &str, expired: bool) -> Vec<u8> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let email = format!("{}@x.com", account.to_lowercase());
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"sub":account,"email":email})).unwrap());
    let expiry = chrono::Utc::now() + chrono::Duration::hours(if expired { -1 } else { 1 });
    serde_json::to_vec_pretty(&json!({
        "token":{"access_token":format!("tok{account}"),"token_type":"Bearer",
            "refresh_token":format!("rt{account}"),"expiry":expiry.to_rfc3339(),
            "expires_in":3600,"extra_token_field":{"keep":true}},
        "auth_method":"consumer","id_token":format!("{header}.{payload}."),
        "extra_root_field":{"keep":[1,2,3]}
    })).unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    mock: mock::Mock,
    a: Vec<u8>,
    b: Vec<u8>,
}

impl Fixture {
    async fn new() -> Self { Self::with_refresh_error(false).await }

    async fn with_refresh_error(reject_b: bool) -> Self {
        let root = tempfile::Builder::new().prefix("agy-cli-")
            .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")).unwrap();
        for dir in ["app/profiles", "home", "claude", "codex", "bin"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        Self { root, mock: mock::Mock::start(reject_b).await, a: blob("A", false), b: blob("B", false) }
    }

    fn path(&self, relative: &str) -> PathBuf { self.root.path().join(relative) }
    fn live(&self) -> Vec<u8> { fs::read(self.path("live.json")).unwrap() }
    fn saved(&self, alias: &str) -> Vec<u8> {
        fs::read(self.path(&format!("app/profiles/{alias}/credentials.json"))).unwrap()
    }
    fn profile(&self, alias: &str, bytes: &[u8]) {
        let dir = self.path("app/profiles").join(alias);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("credentials.json"), bytes).unwrap();
    }
    fn two_profiles(&self) {
        fs::write(self.path("live.json"), &self.a).unwrap();
        self.profile("a", &self.a);
        self.profile("b", &self.b);
    }

    // The only binary constructor: every invocation MUST use the file backend.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_paper-agy-switch"));
        cmd.args(["--color", "never"]).args(args).current_dir(self.root.path())
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .env("PAPER_AGY_SWITCH_HOME", self.path("app"))
            .env("PAPER_AGY_SWITCH_CREDENTIAL_FILE", self.path("live.json"))
            .env("PAPER_AGY_SWITCH_OAUTH_URL", format!("{}/token", self.mock.base))
            .env("PAPER_AGY_SWITCH_CLOUDCODE_URL", &self.mock.base)
            .env("HOME", self.path("home")).env("USERPROFILE", self.path("home"))
            .env("APPDATA", self.path("home")).env("LOCALAPPDATA", self.path("home"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude")).env("CODEX_HOME", self.path("codex"))
            .env("CS_CLAUDE_API_BASE", &self.mock.base)
            .env("CS_CLAUDE_TOKEN_URL", format!("{}/token", self.mock.base))
            .env("CS_TOKEN_URL", format!("{}/token", self.mock.base))
            .env("CS_USAGE_URL", &self.mock.base)
            .env("CS_GITHUB_API_URL", &self.mock.base).env("CS_UPDATE_TTL_SECS", "999999999")
            .env("NO_COLOR", "1").env("NO_PROXY", "*").env("no_proxy", "*")
            .env("PATH", fake_path(&self.path("bin")));
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "CS_PROXY", "RUST_LOG"] {
            cmd.env_remove(key);
        }
        cmd
    }
    fn run(&self, args: &[&str]) -> Output { output_with_timeout(&mut self.command(args)) }
}

fn fake_path(bin: &Path) -> std::ffi::OsString {
    let mut paths = vec![bin.to_path_buf()];
    #[cfg(windows)]
    {
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
        paths.extend([windows.join("System32"), windows]);
    }
    #[cfg(not(windows))]
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    std::env::join_paths(paths).unwrap()
}

fn output_with_timeout(cmd: &mut Command) -> Output {
    let mut child = cmd.spawn().unwrap();
    let drain = |mut pipe: Box<dyn Read + Send>| thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Output { status, stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() };
        }
        if start.elapsed() >= Duration::from_secs(30) {
            child.kill().unwrap(); // Only the child this helper started.
            child.wait().unwrap();
            panic!("CLI timed out; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&stdout.join().unwrap()), String::from_utf8_lossy(&stderr.join().unwrap()));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn text(output: &Output) -> String {
    format!("stdout: {}\nstderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}
fn success(output: &Output) { assert!(output.status.success(), "{}: {}", output.status, text(output)); }
fn rows(output: &Output) -> Vec<Value> {
    success(output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(output)));
    value.as_array().unwrap_or_else(|| panic!("expected JSON array: {value}")).clone()
}
fn row<'a>(rows: &'a [Value], alias: &str) -> &'a Value {
    rows.iter().find(|row| row["alias"] == alias).unwrap_or_else(|| panic!("missing alias {alias}: {rows:?}"))
}
fn no_tokens(output: &Output) {
    for token in ["tokA", "tokB", "rtA", "rtB", "id_token", "access_token", "refresh_token"] {
        assert!(!text(output).contains(token), "leaked synthetic credential {token}: {}", text(output));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row01_login_saves_live_bytes_and_identity() {
    let f = Fixture::new().await;
    fs::write(f.path("live.json"), &f.a).unwrap();
    let out = f.run(&["login", "work"]);
    success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("work") && stdout.contains("a@x.com"), "{}", text(&out));
    assert_eq!(f.saved("work"), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row02_login_without_live_explains_agy_signin() {
    let f = Fixture::new().await;
    let out = f.run(&["login", "work"]);
    assert!(!out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not signed in to Antigravity") && stderr.contains("agy"), "{}", text(&out));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row03_login_refuses_alias_owned_by_another_account() {
    let f = Fixture::new().await;
    f.two_profiles();
    let out = f.run(&["login", "b"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("belongs to"), "{}", text(&out));
    assert_eq!(f.saved("b"), f.b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row04_login_derives_alias_from_email() {
    let f = Fixture::new().await;
    fs::write(f.path("live.json"), &f.a).unwrap();
    success(&f.run(&["login"]));
    assert_eq!(f.saved("a"), f.a);
}

// Usage layout is locked here for GREEN: primary/secondary are the default
// Claude/GPT 5h/weekly windows; gemini contains its corresponding windows.
fn used(value: &Value, expected: f64) {
    let actual = value.as_f64().unwrap_or_else(|| panic!("missing used percent: {value}"));
    assert!((actual - expected).abs() < 0.0001, "expected {expected}, got {actual}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row05_list_json_reports_identity_active_tier_and_both_pools() {
    let f = Fixture::new().await;
    f.two_profiles();
    let entries = rows(&f.run(&["list", "--json"]));
    assert_eq!(entries.len(), 2);
    for (alias, email, active, five, week, gemini) in [
        ("a", "a@x.com", true, 80.0, 20.0, 10.0),
        ("b", "b@x.com", false, 10.0, 5.0, 90.0),
    ] {
        let r = row(&entries, alias);
        assert_eq!(r["email"], email);
        assert_eq!(r["active"], active);
        assert_eq!(r["tier"], "g1-pro-tier");
        used(&r["usage"]["primary"]["used_percent"], five);
        used(&r["usage"]["secondary"]["used_percent"], week);
        used(&r["usage"]["gemini"]["primary"]["used_percent"], gemini);
        used(&r["usage"]["gemini"]["secondary"]["used_percent"], week);
    }
    assert_eq!(f.live(), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row06_list_table_marks_active_and_shows_gemini_without_tokens() {
    let f = Fixture::new().await;
    f.two_profiles();
    let out = f.run(&["list"]);
    success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for email in ["a@x.com", "b@x.com"] { assert!(stdout.contains(email), "{}", text(&out)); }
    for alias in ["a", "b"] {
        assert!(stdout.split_whitespace().any(|word| word.trim_matches(['│','*','●','>']) == alias), "missing alias {alias}: {}", text(&out));
    }
    let active_line = stdout.lines().find(|line| line.contains("a@x.com")).unwrap();
    assert!(["*", "●", "✓", ">", "active", "current"].iter().any(|mark| active_line.contains(mark)), "missing active marker: {active_line}");
    assert!(stdout.contains("Claude") || stdout.contains("5h"), "{}", text(&out));
    assert!(stdout.to_lowercase().contains("gemini"), "{}", text(&out));
    no_tokens(&out);
}

fn assert_refresh(f: &Fixture, alias: &str, original: &[u8], token: &str, refresh: &str) {
    let calls = f.mock.calls();
    let refresh_calls: Vec<_> = calls.iter().filter(|call| call.path == "/token").collect();
    assert_eq!(refresh_calls.len(), 1, "{calls:?}");
    assert!(refresh_calls[0].body.split('&').any(|field| field == format!("refresh_token={refresh}")), "{refresh_calls:?}");
    assert!(calls.iter().any(|call| call.path == "/v1internal:retrieveUserQuotaSummary" && call.token == token), "quota did not use refreshed token: {calls:?}");
    let mut after: Value = serde_json::from_slice(&f.saved(alias)).unwrap();
    let before: Value = serde_json::from_slice(original).unwrap();
    assert_eq!(after["token"]["access_token"], token);
    let expiry = chrono::DateTime::parse_from_rfc3339(after["token"]["expiry"].as_str().unwrap()).unwrap();
    assert!(expiry > chrono::Utc::now());
    after["token"]["access_token"] = before["token"]["access_token"].clone();
    after["token"]["expiry"] = before["token"]["expiry"].clone();
    assert_eq!(after, before, "refresh changed unrelated fields");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row07_list_refreshes_inactive_profile_preserving_other_fields() {
    let f = Fixture::new().await;
    f.two_profiles();
    let expired = blob("B", true);
    f.profile("b", &expired);
    success(&f.run(&["list"]));
    assert_refresh(&f, "b", &expired, "tokB2", "rtB");
    assert_eq!(f.live(), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row08_list_refreshes_active_profile_but_never_live_blob() {
    let f = Fixture::new().await;
    f.two_profiles();
    let expired = blob("A", true);
    f.profile("a", &expired);
    fs::write(f.path("live.json"), &expired).unwrap();
    success(&f.run(&["list"]));
    assert_refresh(&f, "a", &expired, "tokA2", "rtA");
    assert_eq!(f.live(), expired);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row09_revoked_refresh_is_a_row_error_not_list_failure() {
    let f = Fixture::with_refresh_error(true).await;
    f.two_profiles();
    f.profile("b", &blob("B", true));
    let entries = rows(&f.run(&["list", "--json"]));
    assert_eq!(entries.len(), 2);
    let a = row(&entries, "a");
    assert!(a.get("error").is_none_or(Value::is_null), "{a}");
    used(&a["usage"]["primary"]["used_percent"], 80.0);
    let b = row(&entries, "b");
    let diagnostic = [b.get("error"), b.get("status")].into_iter().flatten()
        .map(Value::to_string).collect::<Vec<_>>().join(" ").to_lowercase();
    assert!(diagnostic.contains("re-login") || diagnostic.contains("sign in again"), "{b}");
    let calls = f.mock.calls();
    assert_eq!(calls.iter().filter(|call| call.path == "/token" && call.body.contains("refresh_token=rtB")).count(), 1);
    assert_eq!(f.live(), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row10_use_switches_exact_saved_bytes_and_captures_previous_live() {
    let f = Fixture::new().await;
    f.two_profiles();
    let out = f.run(&["use", "b"]);
    success(&out);
    assert!(String::from_utf8_lossy(&out.stdout).contains('b'), "{}", text(&out));
    assert_eq!(f.live(), f.b);
    assert_eq!(f.saved("a"), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row11_use_captures_fresher_live_before_switch() {
    let f = Fixture::new().await;
    f.two_profiles();
    let mut fresh: Value = serde_json::from_slice(&f.a).unwrap();
    fresh["token"]["access_token"] = json!("tokAfresh");
    fresh["token"]["expiry"] = json!((chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339());
    let fresh = serde_json::to_vec_pretty(&fresh).unwrap();
    fs::write(f.path("live.json"), &fresh).unwrap();
    success(&f.run(&["use", "b"]));
    assert_eq!(f.saved("a"), fresh);
    assert_eq!(f.live(), f.b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row12_use_current_is_a_noop() {
    let f = Fixture::new().await;
    f.two_profiles();
    fs::write(f.path("live.json"), &f.b).unwrap();
    let out = f.run(&["use", "b"]);
    success(&out);
    assert!(String::from_utf8_lossy(&out.stdout).to_lowercase().contains("already active"), "{}", text(&out));
    assert_eq!(f.live(), f.b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row13_use_unknown_alias_leaves_live_unchanged() {
    let f = Fixture::new().await;
    f.two_profiles();
    let out = f.run(&["use", "zzz"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(f.live(), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row14_use_without_alias_selects_claude_gpt_headroom() {
    let f = Fixture::new().await;
    f.two_profiles();
    success(&f.run(&["use"]));
    assert_eq!(f.live(), f.b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row15_use_configured_gemini_pool_decides() {
    let f = Fixture::new().await;
    f.two_profiles();
    fs::write(f.path("app/config.toml"), "[use]\npool = \"gemini\"\n").unwrap();
    success(&f.run(&["use"]));
    assert_eq!(f.live(), f.a);
    let calls = f.mock.calls();
    for token in ["tokA", "tokB"] {
        assert!(calls.iter().any(|c| c.path == "/v1internal:retrieveUserQuotaSummary" && c.token == token), "{calls:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row16_delete_refuses_active_and_archives_restores_inactive() {
    let f = Fixture::new().await;
    f.two_profiles();
    let out = f.run(&["delete", "a", "--yes"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(f.saved("a"), f.a);
    success(&f.run(&["delete", "b", "--yes"]));
    assert!(!f.path("app/profiles/b").exists());
    success(&f.run(&["restore", "b"]));
    assert_eq!(f.saved("b"), f.b);
    assert_eq!(f.live(), f.a);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row17_renamed_profile_can_be_switched_to() {
    let f = Fixture::new().await;
    f.two_profiles();
    success(&f.run(&["rename", "b", "c"]));
    assert!(!f.path("app/profiles/b").exists());
    assert_eq!(f.saved("c"), f.b);
    success(&f.run(&["use", "c"]));
    assert_eq!(f.live(), f.saved("c"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row18_doctor_reports_backend_identity_and_agy_path_status() {
    let f = Fixture::new().await;
    fs::write(f.path("live.json"), &f.a).unwrap();
    let out = f.run(&["doctor"]);
    success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(f.path("live.json").to_str().unwrap()), "{}", text(&out));
    assert!(stdout.contains("a@x.com"), "{}", text(&out));
    assert!(stdout.lines().any(|line| {
        let line = line.to_lowercase();
        line.contains("agy") && ["path", "found", "missing", "installed", "available"].iter().any(|word| line.contains(word))
    }), "missing agy PATH diagnostic: {}", text(&out));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row19_launch_switches_and_passes_args_to_fake_agy() {
    let f = Fixture::new().await;
    f.two_profiles();
    #[cfg(windows)]
    fs::write(f.path("bin/agy.cmd"), "@echo off\r\necho FAKE_AGY_ARGS:%*\r\necho %*>\"%AGY_TEST_ARGS_FILE%\"\r\nexit /b 0\r\n").unwrap();
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(f.path("bin/agy"), "#!/bin/sh\nprintf 'FAKE_AGY_ARGS:%s\\n' \"$*\"\nprintf '%s' \"$*\" > \"$AGY_TEST_ARGS_FILE\"\n").unwrap();
        fs::set_permissions(f.path("bin/agy"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = output_with_timeout(f.command(&["launch", "b", "--", "--version"])
        .env("AGY_TEST_ARGS_FILE", f.path("args.txt")));
    success(&out);
    assert_eq!(f.live(), f.b);
    assert_eq!(fs::read_to_string(f.path("args.txt")).unwrap().trim(), "--version");
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAKE_AGY_ARGS:--version"), "{}", text(&out));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row20_help_contains_no_claude_wording() {
    let f = Fixture::new().await;
    let mut offending = Vec::new();
    for args in [vec!["--help"], vec!["login", "--help"], vec!["use", "--help"], vec!["launch", "--help"], vec!["doctor", "--help"]] {
        let out = f.run(&args);
        success(&out);
        if text(&out).to_lowercase().contains("claude") { offending.push(args.join(" ")); }
    }
    assert!(offending.is_empty(), "Claude wording in: {offending:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row21_disabled_window_is_off_in_table_and_flagged_in_json() {
    let mut f = Fixture::new().await;
    f.mock = mock::Mock::start_with_disabled(false, true).await;
    f.two_profiles();
    let entries = rows(&f.run(&["list", "--json"]));
    for alias in ["a", "b"] {
        let r = row(&entries, alias);
        assert_eq!(r["usage"]["primary"]["disabled"], true);
        assert_eq!(r["usage"]["secondary"]["disabled"], false);
        assert_eq!(r["usage"]["gemini"]["primary"]["disabled"], false);
    }
    let out = f.run(&["list"]);
    success(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for email in ["a@x.com", "b@x.com"] {
        let line = stdout.lines().find(|line| line.contains(email)).unwrap();
        assert!(line.contains("off ↻"), "{line}");
    }
}
