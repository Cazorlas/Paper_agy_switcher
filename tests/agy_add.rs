//! Browser OAuth contract, using synthetic JWTs and loopback endpoints only.
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc},
    thread,
    time::{Duration, Instant},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn id_token(email: &str) -> String {
    format!("e30.{}.", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"sub":email,"email":email})).unwrap()))
}

struct Fixture {
    root: tempfile::TempDir,
    url: String,
    calls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new(with_refresh: bool) -> Self {
        let root = tempfile::tempdir_in(concat!(env!("CARGO_MANIFEST_DIR"), "/target")).unwrap();
        fs::create_dir_all(root.path().join("app/profiles")).unwrap();
        fs::write(root.path().join("live.json"), b"untouched live credential").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_calls = calls.clone();
        let thread_stop = stop.clone();
        let server = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => { thread::sleep(Duration::from_millis(10)); continue; }
                    Err(e) => panic!("{e}"),
                };
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /token "));
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" { break; }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") { length = value.trim().parse().unwrap(); }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                thread_calls.lock().unwrap().push(String::from_utf8(body).unwrap());
                let mut response = json!({"access_token":"synthetic-access","refresh_token":"synthetic-refresh","id_token":id_token("new@example.com"),"expires_in":3600});
                if !with_refresh { response.as_object_mut().unwrap().remove("refresh_token"); }
                let response = response.to_string();
                write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        Self { root, url, calls, stop, server: Some(server) }
    }

    fn start(&self, alias: Option<&str>, timeout: u64) -> (Running, reqwest::Url) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_paper-agy-switch"));
        cmd.args(["--color", "never", "add"]);
        if let Some(alias) = alias { cmd.arg(alias); }
        cmd.args(["--timeout-secs", &timeout.to_string()])
            .env("PAPER_AGY_SWITCH_HOME", self.root.path().join("app"))
            .env("PAPER_AGY_SWITCH_CREDENTIAL_FILE", self.root.path().join("live.json"))
            .env("PAPER_AGY_SWITCH_OAUTH_URL", &self.url)
            .env("PAPER_AGY_SWITCH_NO_BROWSER", "1")
            .env("CS_PROXY", "").env("NO_PROXY", "localhost,127.0.0.1")
            .env_remove("HTTP_PROXY").env_remove("HTTPS_PROXY").env_remove("ALL_PROXY")
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = Running(cmd.spawn().unwrap());
        let stdout = child.0.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let _ = tx.send((line, reader));
        });
        let (line, mut reader) = rx.recv_timeout(Duration::from_secs(8)).expect("auth URL not printed");
        let url = reqwest::Url::parse(line.trim().strip_prefix("AUTH_URL ").expect("AUTH_URL line")).unwrap();
        // Drain the remaining small diagnostic output while the child runs.
        thread::spawn(move || { let mut rest = String::new(); let _ = reader.read_to_string(&mut rest); });
        (child, url)
    }

    fn saved(&self, alias: &str) -> std::path::PathBuf { self.root.path().join(format!("app/profiles/{alias}/credentials.json")) }
    fn assert_live_untouched(&self) { assert_eq!(fs::read(self.root.path().join("live.json")).unwrap(), b"untouched live credential"); }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.server.take().unwrap().join().unwrap();
    }
}

struct Running(Child);
impl Running {
    fn finish(mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                let mut stderr = Vec::new();
                self.0.stderr.take().unwrap().read_to_end(&mut stderr).unwrap();
                return Output { status, stdout: Vec::new(), stderr };
            }
            assert!(Instant::now() < deadline, "add failed to exit within 8 seconds");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Running { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

fn fields(url: &reqwest::Url) -> HashMap<String, String> { url.query_pairs().into_owned().collect() }
fn callback(auth: &reqwest::Url, path: &str, state: Option<&str>, tail: &str) -> String {
    let params = fields(auth);
    let mut url = reqwest::Url::parse(&params["redirect_uri"]).unwrap();
    url.set_path(path);
    url.set_query(Some(tail));
    if let Some(state) = state { url.query_pairs_mut().append_pair("state", state); }
    let mut stream = TcpStream::connect(("127.0.0.1", url.port().unwrap())).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(stream, "GET {}?{} HTTP/1.1\r\nHost: localhost\r\n\r\n", url.path(), url.query().unwrap()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn happy_path_scopes_pkce_exact_blob_and_live_untouched() {
    let f = Fixture::new(true);
    let (child, auth) = f.start(Some("second"), 5);
    let params = fields(&auth);
    assert_eq!(auth.as_str().split('?').next().unwrap(), "https://accounts.google.com/o/oauth2/v2/auth");
    for (key, value) in [("access_type", "offline"), ("prompt", "consent"), ("code_challenge_method", "S256"), ("response_type", "code"), ("client_id", agy_switch::agy_api::CLIENT_ID)] { assert_eq!(params[key], value); }
    let scopes: Vec<_> = params["scope"].split_whitespace().collect();
    assert_eq!(scopes, ["openid", "https://www.googleapis.com/auth/cloud-platform", "https://www.googleapis.com/auth/userinfo.email", "https://www.googleapis.com/auth/userinfo.profile", "https://www.googleapis.com/auth/cclog", "https://www.googleapis.com/auth/experimentsandconfigs", "https://www.googleapis.com/auth/aicode"]);
    assert!(URL_SAFE_NO_PAD.decode(&params["state"]).unwrap().len() >= 16);
    assert!(callback(&auth, "/favicon.ico", None, "").starts_with("HTTP/1.1 404"));
    assert!(callback(&auth, "/oauth-callback", Some(&params["state"]), "code=fake-code").contains("Return to the terminal"));
    let out = child.finish();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let blob = fs::read_to_string(f.saved("second")).unwrap();
    let value: Value = serde_json::from_str(&blob).unwrap();
    let expiry = value["token"]["expiry"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(expiry).unwrap() > chrono::Utc::now());
    assert_eq!(blob, format!("{{\"token\":{{\"access_token\":\"synthetic-access\",\"token_type\":\"Bearer\",\"refresh_token\":\"synthetic-refresh\",\"expiry\":\"{expiry}\",\"expires_in\":3600}},\"auth_method\":\"consumer\",\"id_token\":\"{}\"}}", id_token("new@example.com")));
    assert_eq!(agy_switch::agy_store::identity_of(blob.as_bytes()).unwrap().email.as_deref(), Some("new@example.com"));
    let calls = f.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let form: HashMap<String, String> = reqwest::Url::parse(&format!("http://localhost/?{}", calls[0])).unwrap().query_pairs().into_owned().collect();
    assert_eq!(form.len(), 6);
    assert_eq!(form["code"], "fake-code");
    assert_eq!(form["redirect_uri"], params["redirect_uri"]);
    assert_eq!(form["client_id"], agy_switch::agy_api::CLIENT_ID);
    assert_eq!(form["client_secret"], agy_switch::agy_api::CLIENT_SECRET);
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())), params["code_challenge"]);
    assert!((43..=128).contains(&form["code_verifier"].len()));
    f.assert_live_untouched();
}

#[test]
fn rejects_state_mismatch_missing_state_and_access_denied() {
    for (state, tail, error) in [(Some("wrong"), "code=fake", "state"), (None, "code=fake", "state"), (Some("valid"), "error=access_denied", "access_denied")] {
        let f = Fixture::new(true);
        let (child, auth) = f.start(Some("second"), 5);
        let params = fields(&auth);
        let state = state.map(|s| if s == "valid" { params["state"].as_str() } else { s });
        assert!(callback(&auth, "/oauth-callback", state, tail).starts_with("HTTP/1.1 400"));
        let out = child.finish();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(error));
        assert!(!f.saved("second").exists());
        assert!(f.calls.lock().unwrap().is_empty());
        f.assert_live_untouched();
    }
}

#[test]
fn missing_refresh_token_aborts() {
    let f = Fixture::new(false);
    let (child, auth) = f.start(Some("second"), 5);
    callback(&auth, "/oauth-callback", Some(&fields(&auth)["state"]), "code=fake");
    let out = child.finish();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing refresh_token"));
    assert!(!f.saved("second").exists());
    f.assert_live_untouched();
}

#[test]
fn refuses_alias_owned_by_another_identity() {
    let f = Fixture::new(true);
    let old = json!({"id_token":id_token("old@example.com"),"token":{}}).to_string();
    fs::create_dir_all(f.saved("second").parent().unwrap()).unwrap();
    fs::write(f.saved("second"), &old).unwrap();
    let (child, auth) = f.start(Some("second"), 5);
    callback(&auth, "/oauth-callback", Some(&fields(&auth)["state"]), "code=fake");
    let out = child.finish();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("belongs to another account"));
    assert_eq!(fs::read_to_string(f.saved("second")).unwrap(), old);
    f.assert_live_untouched();
}

#[test]
fn derives_alias_and_honours_timeout() {
    let f = Fixture::new(true);
    let (child, auth) = f.start(None, 5);
    callback(&auth, "/oauth-callback", Some(&fields(&auth)["state"]), "code=fake");
    assert!(child.finish().status.success());
    assert!(f.saved("new").exists());
    let started = Instant::now();
    let (child, _) = f.start(Some("timeout"), 1);
    let out = child.finish();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(!f.saved("timeout").exists());
    f.assert_live_untouched();
}
