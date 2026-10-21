//! Configuration boundary checks retained after replacing Claude CLI contracts.
use std::{fs, process::Command};
use serde_json::Value;

fn invalid_config(raw: &str) -> std::process::Output {
    let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    fs::write(root.path().join("config.toml"), raw).unwrap();
    Command::new(env!("CARGO_BIN_EXE_paper-agy-switch"))
        .args(["--json", "list"]).current_dir(root.path())
        .env("PAPER_AGY_SWITCH_HOME", root.path())
        .env("PAPER_AGY_SWITCH_CREDENTIAL_FILE", root.path().join("live.json"))
        .env("PAPER_AGY_SWITCH_OAUTH_URL", "http://127.0.0.1:1/token")
        .env("PAPER_AGY_SWITCH_CLOUDCODE_URL", "http://127.0.0.1:1")
        .env("HOME", root.path()).env("USERPROFILE", root.path())
        .env("NO_COLOR", "1").output().unwrap()
}

#[test]
fn invalid_existing_config_fails_instead_of_using_defaults() {
    let output = invalid_config("[network]\nmax_concurrent = \"many\"\n");
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(report["error"].as_str().unwrap().contains("config.toml"));
    assert!(report["error"].as_str().unwrap().contains("parse"));
}

#[test]
fn invalid_config_error_does_not_echo_proxy_credentials() {
    let output = invalid_config("[proxy]\nurl = \"http://user:SENTINEL_PASSWORD@example.com\n");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("failed to parse config file"));
    assert!(!stdout.contains("SENTINEL_PASSWORD"));
    assert!(!stderr.contains("SENTINEL_PASSWORD"));
}

#[test]
fn invalid_scoring_pool_fails_at_configuration_boundary() {
    let output = invalid_config("[use]\npool = \"unknown\"\n");
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["error"].as_str().unwrap().contains("parse"));
}
