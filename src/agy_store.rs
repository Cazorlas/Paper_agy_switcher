//! Antigravity login stored in the OS credential store, and saved profiles.
use std::{fs, io, path::PathBuf};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

pub const DEFAULT_TARGET: &str = "gemini:antigravity";
pub const CREDENTIAL_USER: &str = "antigravity";
pub const CREDENTIAL_FILE_ENV: &str = "PAPER_AGY_SWITCH_CREDENTIAL_FILE";

pub trait CredentialBackend: Send + Sync {
    fn read(&self) -> Result<Option<Vec<u8>>>;
    fn write(&self, blob: &[u8]) -> Result<()>;
    fn describe(&self) -> String;
}

#[cfg(windows)]
pub struct WindowsCredential { pub target: String }

#[cfg(windows)]
impl WindowsCredential {
    fn wide_target(&self) -> Result<Vec<u16>> {
        if self.target.contains('\0') { bail!("credential target contains a null character"); }
        Ok(self.target.encode_utf16().chain(Some(0)).collect())
    }

    pub fn delete(&self) -> Result<()> {
        use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
        use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
        let target = self.wide_target()?;
        // The terminated target buffer lives through the call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NOT_FOUND as i32) {
                return Err(error).context("deleting Windows credential");
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl CredentialBackend for WindowsCredential {
    fn read(&self) -> Result<Option<Vec<u8>>> {
        use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
        use windows_sys::Win32::Security::Credentials::{CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC};
        struct Buffer(*mut CREDENTIALW);
        impl Drop for Buffer {
            fn drop(&mut self) {
                // CredReadW allocations must be released by CredFree.
                unsafe { CredFree(self.0.cast()) };
            }
        }
        let target = self.wide_target()?;
        let mut pointer = std::ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) { return Ok(None); }
            return Err(error).context("reading Windows credential");
        }
        let buffer = Buffer(pointer);
        // The structure and its blob stay valid until buffer is dropped.
        let credential = unsafe { &*buffer.0 };
        let bytes = if credential.CredentialBlobSize == 0 { Vec::new() } else {
            unsafe { std::slice::from_raw_parts(credential.CredentialBlob, credential.CredentialBlobSize as usize) }.to_vec()
        };
        Ok(Some(bytes))
    }

    fn write(&self, blob: &[u8]) -> Result<()> {
        use windows_sys::Win32::Security::Credentials::{CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC};
        let mut target = self.wide_target()?;
        let mut user: Vec<u16> = CREDENTIAL_USER.encode_utf16().chain(Some(0)).collect();
        let mut bytes = blob.to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            UserName: user.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            CredentialBlobSize: bytes.len().try_into().context("credential blob is too large")?,
            CredentialBlob: bytes.as_mut_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        // All referenced buffers live through this synchronous call.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(io::Error::last_os_error()).context("writing Windows credential");
        }
        Ok(())
    }

    fn describe(&self) -> String { format!("Windows Credential Manager ({})", self.target) }
}

pub struct FileCredential { pub path: PathBuf }
impl CredentialBackend for FileCredential {
    fn read(&self) -> Result<Option<Vec<u8>>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("reading credential file"),
        }
    }
    fn write(&self, blob: &[u8]) -> Result<()> { crate::auth::atomic_write_private(&self.path, blob) }
    fn describe(&self) -> String { format!("Credential file ({})", self.path.display()) }
}

pub fn backend_from_env() -> Result<Box<dyn CredentialBackend>> {
    if let Some(path) = std::env::var_os(CREDENTIAL_FILE_ENV).filter(|path| !path.is_empty()) {
        return Ok(Box::new(FileCredential { path: path.into() }));
    }
    #[cfg(windows)]
    { Ok(Box::new(WindowsCredential { target: DEFAULT_TARGET.to_owned() })) }
    #[cfg(not(windows))]
    { bail!("Antigravity credentials are not supported on this OS yet") }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity { pub sub: Option<String>, pub email: Option<String> }

fn parse_blob(blob: &[u8]) -> Result<Value> {
    // JSON diagnostics can quote credential values, so discard parser details.
    let value: Value = serde_json::from_slice(blob).map_err(|_| anyhow::anyhow!("credential blob is not valid JSON"))?;
    if !value.is_object() { bail!("credential blob must be a JSON object"); }
    Ok(value)
}

pub fn identity_of(blob: &[u8]) -> Result<Identity> {
    let value = parse_blob(blob)?;
    let Some(token) = value.get("id_token") else { return Ok(Identity::default()); };
    let token = token.as_str().context("credential id_token must be a string")?;
    let payload = token.split('.').nth(1).context("credential id_token has no JWT payload")?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).map_err(|_| anyhow::anyhow!("credential JWT payload is not base64url"))?;
    let claims: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("credential JWT payload is not valid JSON"))?;
    let field = |name: &str| claims.get(name).and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned);
    Ok(Identity { sub: field("sub"), email: field("email") })
}

pub fn same_account(a: &Identity, b: &Identity) -> bool {
    if let (Some(a), Some(b)) = (&a.sub, &b.sub) { return !a.is_empty() && a == b; }
    match (&a.email, &b.email) {
        (Some(a), Some(b)) => !a.is_empty() && a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

pub fn apply_refresh(blob: &[u8], response: &Value, now: DateTime<Utc>) -> Result<Vec<u8>> {
    let mut value = parse_blob(blob)?;
    let access = response.get("access_token").and_then(Value::as_str).context("refresh response is missing access_token")?;
    let seconds = response.get("expires_in").and_then(Value::as_i64).filter(|seconds| *seconds >= 0).context("refresh response has invalid expires_in")?;
    let expiry = chrono::Duration::try_seconds(seconds).and_then(|duration| now.checked_add_signed(duration)).context("refresh expiry is out of range")?;
    let token = value.get_mut("token").and_then(Value::as_object_mut).context("credential blob is missing token object")?;
    token.insert("access_token".into(), Value::String(access.to_owned()));
    token.insert("expires_in".into(), Value::from(seconds));
    token.insert("expiry".into(), Value::String(expiry.to_rfc3339_opts(SecondsFormat::Secs, true)));
    for key in ["token_type", "refresh_token"] {
        if let Some(replacement) = response.get(key) { token.insert(key.into(), replacement.clone()); }
    }
    if let Some(replacement) = response.get("id_token") { value["id_token"] = replacement.clone(); }
    serde_json::to_vec(&value).context("serializing refreshed credential")
}

pub fn access_token_valid(blob: &[u8], now: DateTime<Utc>) -> Result<bool> {
    let value = parse_blob(blob)?;
    let token = &value["token"];
    if !token["access_token"].as_str().is_some_and(|token| !token.is_empty()) { return Ok(false); }
    let expiry = token["expiry"].as_str().and_then(|expiry| DateTime::parse_from_rfc3339(expiry).ok());
    Ok(expiry.is_some_and(|expiry| expiry.signed_duration_since(now) > chrono::Duration::minutes(5)))
}

pub struct AgyStore { profiles_dir: PathBuf, backend: Box<dyn CredentialBackend> }
#[derive(Debug, PartialEq, Eq)]
pub enum SaveAction { Created, Updated }
#[derive(Debug, PartialEq, Eq)]
pub enum SwitchOutcome { Switched { captured: Option<String> }, AlreadyActive }

impl AgyStore {
    pub fn new(profiles_dir: PathBuf, backend: Box<dyn CredentialBackend>) -> Self { Self { profiles_dir, backend } }
    pub fn read_live(&self) -> Result<Option<Vec<u8>>> { self.backend.read() }

    // lock_live_auth uses the global app home, not this store's profiles dir;
    // using it would make isolated temporary stores write to the real home.
    pub fn save_current(&self, alias: &str) -> Result<SaveAction> {
        crate::profile::validate_alias(alias)?;
        let live = self.read_live()?.context("not signed in to Antigravity")?;
        let identity = identity_of(&live)?;
        let action = match fs::read(self.profile_path(alias)?) {
            Ok(previous) => {
                if !same_account(&identity_of(&previous)?, &identity) { bail!("profile belongs to another account"); }
                SaveAction::Updated
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => SaveAction::Created,
            Err(error) => return Err(error).context("reading saved credential"),
        };
        self.store_profile_blob(alias, &live)?;
        Ok(action)
    }

    pub fn switch_to(&self, alias: &str) -> Result<SwitchOutcome> {
        let target = self.profile_blob(alias)?;
        let target_identity = identity_of(&target)?;
        let live = self.read_live()?;
        let mut captured = None;
        if let Some(live) = &live {
            let identity = identity_of(live)?;
            if same_account(&identity, &target_identity) {
                self.store_profile_blob(alias, live)?;
                return Ok(SwitchOutcome::AlreadyActive);
            }
            if let Some(alias) = self.alias_for(&identity)? {
                self.store_profile_blob(&alias, live)?;
                captured = Some(alias);
            }
        }
        self.backend.write(&target)?;
        if self.backend.read()?.as_deref() != Some(target.as_slice()) { bail!("credential verification failed after switching accounts"); }
        Ok(SwitchOutcome::Switched { captured })
    }

    pub fn active_alias(&self) -> Result<Option<String>> {
        match self.read_live()? {
            Some(live) => self.alias_for(&identity_of(&live)?),
            None => Ok(None),
        }
    }
    pub fn profile_blob(&self, alias: &str) -> Result<Vec<u8>> {
        fs::read(self.profile_path(alias)?).context("reading saved credential")
    }
    pub fn store_profile_blob(&self, alias: &str, blob: &[u8]) -> Result<()> {
        crate::auth::atomic_write_private(&self.profile_path(alias)?, blob)
    }
    fn profile_path(&self, alias: &str) -> Result<PathBuf> {
        crate::profile::validate_alias(alias)?;
        Ok(self.profiles_dir.join(alias).join("credentials.json"))
    }
    fn alias_for(&self, identity: &Identity) -> Result<Option<String>> {
        let entries = match fs::read_dir(&self.profiles_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("reading profiles directory"),
        };
        let mut aliases = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let Ok(alias) = entry.file_name().into_string() else { continue };
                if crate::profile::validate_alias(&alias).is_err() { continue; }
                aliases.push(alias);
            }
        }
        aliases.sort();
        for alias in aliases {
            let Ok(blob) = self.profile_blob(&alias) else { continue };
            let Ok(saved_identity) = identity_of(&blob) else { continue };
            if same_account(identity, &saved_identity) { return Ok(Some(alias)); }
        }
        Ok(None)
    }
}
