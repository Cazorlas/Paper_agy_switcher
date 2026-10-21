use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use agy_switch::agy_store::{
    AgyStore, CREDENTIAL_FILE_ENV, CredentialBackend, DEFAULT_TARGET, FileCredential, Identity,
    SaveAction, SwitchOutcome, access_token_valid, apply_refresh, backend_from_env, identity_of,
    same_account,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tempfile::TempDir;

fn temp_dir() -> TempDir {
    tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap()
}

fn jwt(sub: &str, email: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"sub": sub, "email": email})).unwrap());
    format!("{header}.{payload}.sig")
}

fn blob(sub: &str, email: &str, label: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "token": {
            "access_token": format!("fake-access-{label}"),
            "token_type": "Bearer",
            "refresh_token": format!("fake-refresh-{label}"),
            "expiry": "2030-01-01T01:00:00Z",
            "expires_in": 3600
        },
        "auth_method": "consumer",
        "id_token": jwt(sub, email)
    })).unwrap()
}

fn now() -> DateTime<Utc> {
    "2030-01-01T00:00:00Z".parse().unwrap()
}

#[derive(Clone, Default)]
struct MemoryCredential(Arc<Mutex<LiveState>>);

#[derive(Default)]
struct LiveState {
    blob: Option<Vec<u8>>,
    writes: usize,
}

impl CredentialBackend for MemoryCredential {
    fn read(&self) -> anyhow::Result<Option<Vec<u8>>> { Ok(self.0.lock().unwrap().blob.clone()) }
    fn write(&self, blob: &[u8]) -> anyhow::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.blob = Some(blob.to_vec());
        state.writes += 1;
        Ok(())
    }
    fn describe(&self) -> String { "in-memory test credential".to_owned() }
}

struct Fixture {
    root: TempDir,
    profiles: PathBuf,
    live: MemoryCredential,
    store: AgyStore,
}

impl Fixture {
    fn new(live: Option<Vec<u8>>) -> Self {
        let root = temp_dir();
        let profiles = root.path().join("profiles");
        fs::create_dir(&profiles).unwrap();
        let backend = MemoryCredential(Arc::new(Mutex::new(LiveState { blob: live, writes: 0 })));
        let store = AgyStore::new(profiles.clone(), Box::new(backend.clone()));
        Self { root, profiles, live: backend, store }
    }
    fn seed(&self, alias: &str, bytes: &[u8]) {
        let dir = self.profiles.join(alias);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("credentials.json"), bytes).unwrap();
    }
    fn saved(&self, alias: &str) -> Vec<u8> {
        fs::read(self.profiles.join(alias).join("credentials.json")).unwrap()
    }
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut saved: Vec<_> = fs::read_dir(&self.profiles).unwrap().map(|entry| {
            let path = entry.unwrap().path().join("credentials.json");
            (path.strip_prefix(self.root.path()).unwrap().to_path_buf(), fs::read(path).unwrap())
        }).collect();
        saved.sort();
        saved
    }
}

#[test]
fn identity_reads_unsigned_jwt_payload() {
    assert_eq!(identity_of(&blob("111", "A@x.com", "a")).unwrap(), Identity {
        sub: Some("111".into()), email: Some("A@x.com".into()),
    });
}

#[test]
fn identity_without_id_token_is_empty() {
    assert_eq!(identity_of(br#"{"token":{"access_token":"fake"}}"#).unwrap(), Identity::default());
}

#[test]
fn identity_rejects_non_json() {
    assert!(identity_of(b"not json").is_err());
}

#[test]
fn same_account_prefers_subject_then_case_insensitive_email() {
    let identity = |sub: Option<&str>, email: Option<&str>| Identity {
        sub: sub.map(str::to_owned), email: email.map(str::to_owned),
    };
    assert!(same_account(&identity(Some("1"), Some("a@x.com")), &identity(Some("1"), Some("b@x.com"))));
    assert!(!same_account(&identity(Some("1"), Some("a@x.com")), &identity(Some("2"), Some("a@x.com"))));
    assert!(same_account(&identity(None, Some("A@x.com")), &identity(None, Some("a@X.com"))));
    assert!(!same_account(&Identity::default(), &Identity::default()));
}

#[test]
fn refresh_preserves_unowned_fields_and_top_level_order() {
    let original = br#"{"zeta":7,"token":{"access_token":"old","token_type":"old-type","refresh_token":"r1","expiry":"2029-01-01T00:00:00Z","expires_in":1,"x_extra":{"k":1},"scope":["email"]},"auth_method":"consumer","id_token":"original-jwt","x_extra":{"k":1},"alpha":2}"#;
    let before: Value = serde_json::from_slice(original).unwrap();
    let bytes = apply_refresh(original, &json!({"access_token":"new","expires_in":3600,"token_type":"Bearer"}), now()).unwrap();
    let after: Value = serde_json::from_slice(&bytes).unwrap();
    let mut expected = before.clone();
    expected["token"]["access_token"] = json!("new");
    expected["token"]["token_type"] = json!("Bearer");
    expected["token"]["expiry"] = json!("2030-01-01T01:00:00Z");
    expected["token"]["expires_in"] = json!(3600);
    assert_eq!(after, expected);
    assert_eq!(after.as_object().unwrap().keys().collect::<Vec<_>>(), before.as_object().unwrap().keys().collect::<Vec<_>>());
}

#[test]
fn refresh_replaces_rotated_refresh_and_id_tokens() {
    let bytes = apply_refresh(&blob("1", "a@x.com", "old"), &json!({
        "access_token":"new", "expires_in":3600, "token_type":"Bearer", "refresh_token":"r2", "id_token":"j2"
    }), now()).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["token"]["refresh_token"], "r2");
    assert_eq!(value["id_token"], "j2");
}

#[test]
fn access_token_with_ten_minutes_left_is_valid() {
    assert!(access_token_valid(br#"{"token":{"access_token":"fake","expiry":"2030-01-01T00:10:00Z"}}"#, now()).unwrap());
}

#[test]
fn access_token_with_four_minutes_left_needs_refresh() {
    assert!(!access_token_valid(br#"{"token":{"access_token":"fake","expiry":"2030-01-01T00:04:00Z"}}"#, now()).unwrap());
}

#[test]
fn access_token_parses_go_offset_and_seven_fractional_digits() {
    let now = "2026-10-21T12:01:56Z".parse().unwrap();
    assert!(access_token_valid(br#"{"token":{"access_token":"fake","expiry":"2026-10-21T19:11:56.2113374+07:00"}}"#, now).unwrap());
}

#[test]
fn access_token_parses_six_fractional_digits_in_utc() {
    assert!(access_token_valid(br#"{"token":{"access_token":"fake","expiry":"2030-01-01T00:10:00.000000Z"}}"#, now()).unwrap());
}

#[test]
fn access_token_with_missing_expiry_is_invalid() {
    assert!(!access_token_valid(br#"{"token":{"access_token":"fake"}}"#, now()).unwrap());
}

#[test]
fn access_token_with_unparseable_expiry_is_invalid() {
    assert!(!access_token_valid(br#"{"token":{"access_token":"fake","expiry":"bad date"}}"#, now()).unwrap());
}

#[test]
fn file_credential_round_trips_exact_bytes() {
    let root = temp_dir();
    let backend = FileCredential { path: root.path().join("credential.json") };
    let bytes = b"  {\"token\":{\"access_token\":\"fake\"}}\n";
    backend.write(bytes).unwrap();
    assert_eq!(backend.read().unwrap(), Some(bytes.to_vec()));
    assert_eq!(fs::read(&backend.path).unwrap(), bytes);
}

#[test]
fn file_credential_missing_entry_is_none() {
    let root = temp_dir();
    assert_eq!(FileCredential { path: root.path().join("missing") }.read().unwrap(), None);
}

#[test]
fn backend_env_override_names_file_in_child_process() {
    const CHILD: &str = "PAPER_AGY_STORE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let path = std::env::var_os(CREDENTIAL_FILE_ENV).unwrap();
        let description = backend_from_env().unwrap().describe();
        assert!(description.contains(PathBuf::from(path).to_str().unwrap()), "{description}");
        return;
    }
    let root = temp_dir();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "backend_env_override_names_file_in_child_process", "--nocapture"])
        .env(CHILD, "1")
        .env(CREDENTIAL_FILE_ENV, root.path().join("isolated-credential.json"))
        .output().unwrap();
    assert!(output.status.success(), "child failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn save_current_creates_then_updates_exact_profile_bytes() {
    let bytes = blob("A", "a@x.com", "live");
    let f = Fixture::new(Some(bytes.clone()));
    assert_eq!(f.store.read_live().unwrap(), Some(bytes.clone()));
    assert!(matches!(f.store.save_current("a").unwrap(), SaveAction::Created));
    assert_eq!(f.store.profile_blob("a").unwrap(), bytes);
    assert_eq!(f.saved("a"), bytes);
    assert!(matches!(f.store.save_current("a").unwrap(), SaveAction::Updated));
    assert_eq!(f.saved("a"), bytes);
}

#[test]
fn save_current_requires_signed_in_entry() {
    let f = Fixture::new(None);
    let error = f.store.save_current("a").err().expect("must reject signed-out save");
    assert!(format!("{error:#}").contains("not signed in"), "{error:#}");
    assert!(f.snapshot().is_empty());
}

#[test]
fn save_current_rejects_alias_belonging_to_another_identity() {
    let f = Fixture::new(Some(blob("A", "a@x.com", "live")));
    f.seed("b", &blob("B", "b@x.com", "saved"));
    let before = f.snapshot();
    let error = f.store.save_current("b").err().expect("must reject different account");
    assert!(format!("{error:#}").contains("belongs to"), "{error:#}");
    assert_eq!(f.snapshot(), before);
    assert_eq!(f.live.0.lock().unwrap().writes, 0);
}

#[test]
fn switch_captures_fresh_live_bytes_before_replacing_live() {
    let fresh = blob("A", "a@x.com", "fresh");
    let b = blob("B", "b@x.com", "saved");
    let f = Fixture::new(Some(fresh.clone()));
    f.seed("a", &blob("A", "a@x.com", "old"));
    f.seed("b", &b);
    assert!(matches!(f.store.switch_to("b").unwrap(), SwitchOutcome::Switched { captured: Some(alias) } if alias == "a"));
    assert_eq!(f.live.read().unwrap(), Some(b.clone()));
    assert_eq!(f.saved("a"), fresh);
    assert_eq!(f.saved("b"), b);
}

#[test]
fn switch_already_active_updates_saved_profile_without_live_write() {
    let live = blob("B", "b@x.com", "fresh");
    let f = Fixture::new(Some(live.clone()));
    f.seed("b", &blob("B", "b@x.com", "old"));
    assert!(matches!(f.store.switch_to("b").unwrap(), SwitchOutcome::AlreadyActive));
    assert_eq!(f.live.read().unwrap(), Some(live.clone()));
    assert_eq!(f.live.0.lock().unwrap().writes, 0);
    assert_eq!(f.saved("b"), live);
}

#[test]
fn switch_unsaved_live_account_leaves_all_profiles_unchanged() {
    let b = blob("B", "b@x.com", "saved");
    let f = Fixture::new(Some(blob("C", "c@x.com", "unsaved")));
    f.seed("a", &blob("A", "a@x.com", "saved"));
    f.seed("b", &b);
    let before = f.snapshot();
    assert!(matches!(f.store.switch_to("b").unwrap(), SwitchOutcome::Switched { captured: None }));
    assert_eq!(f.live.read().unwrap(), Some(b));
    assert_eq!(f.snapshot(), before);
}

#[test]
fn switch_unknown_alias_leaves_live_untouched() {
    let live = blob("A", "a@x.com", "live");
    let f = Fixture::new(Some(live.clone()));
    f.seed("a", &live);
    let before = f.snapshot();
    assert!(f.store.switch_to("zzz").is_err());
    assert_eq!(f.live.read().unwrap(), Some(live));
    assert_eq!(f.live.0.lock().unwrap().writes, 0);
    assert_eq!(f.snapshot(), before);
}

#[test]
fn active_alias_matches_identity_rather_than_token_bytes() {
    let f = Fixture::new(Some(blob("A", "a@x.com", "fresh")));
    f.seed("a", &blob("A", "a@x.com", "old"));
    f.seed("b", &blob("B", "b@x.com", "saved"));
    assert_eq!(f.store.active_alias().unwrap(), Some("a".into()));
}

#[test]
fn active_alias_is_none_for_unknown_live_account() {
    let f = Fixture::new(Some(blob("C", "c@x.com", "unsaved")));
    f.seed("a", &blob("A", "a@x.com", "saved"));
    assert_eq!(f.store.active_alias().unwrap(), None);
}

#[test]
fn active_alias_is_none_without_live_entry() {
    let f = Fixture::new(None);
    f.seed("a", &blob("A", "a@x.com", "saved"));
    assert_eq!(f.store.read_live().unwrap(), None);
    assert_eq!(f.store.active_alias().unwrap(), None);
}

#[test]
fn store_profile_blob_uses_alias_credentials_path_and_preserves_bytes() {
    let f = Fixture::new(None);
    let bytes = blob("A", "a@x.com", "stored");
    f.store.store_profile_blob("a", &bytes).unwrap();
    assert_eq!(f.saved("a"), bytes);
    assert_eq!(f.store.profile_blob("a").unwrap(), bytes);
    assert_eq!(f.live.0.lock().unwrap().writes, 0);
}

#[cfg(windows)]
#[test]
fn windows_credential_round_trip_1500_bytes_and_delete() {
    use agy_switch::agy_store::WindowsCredential;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Cleanup {
        credential: WindowsCredential,
        deleted: bool,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if self.deleted { return; }
            // RED stubs panic too: do not double-panic during unwinding.
            let result = std::panic::catch_unwind(|| self.credential.delete());
            if !std::thread::panicking() {
                match result {
                    Ok(result) => result.expect("clean up isolated credential"),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
        }
    }
    let target = format!("paper-agy-switch-test:{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
    assert_ne!(target, DEFAULT_TARGET);
    let mut cleanup = Cleanup { credential: WindowsCredential { target }, deleted: false };
    let bytes: Vec<u8> = (0..1500).map(|n| (n % 256) as u8).collect();
    cleanup.credential.write(&bytes).unwrap();
    assert_eq!(cleanup.credential.read().unwrap(), Some(bytes));
    cleanup.credential.delete().unwrap();
    cleanup.deleted = true;
    assert_eq!(cleanup.credential.read().unwrap(), None);
}

#[test]
fn safety_tests_never_spell_live_credential_target() {
    // A stricter guard than construction scanning: use DEFAULT_TARGET for comparisons.
    for entry in fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let source = fs::read_to_string(&path).unwrap();
            assert!(!source.contains(DEFAULT_TARGET), "live target literal in {}", path.display());
        }
    }
}
