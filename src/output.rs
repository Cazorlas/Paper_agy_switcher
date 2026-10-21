use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Error;
use chrono::{DateTime, Local, TimeZone};
use serde::Serialize;


/// Marker error: the command already printed a user-facing failure message.
#[derive(Debug)]
pub struct OutputAlreadyReported;

impl std::fmt::Display for OutputAlreadyReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("command failed; details were already reported")
    }
}

impl std::error::Error for OutputAlreadyReported {}

pub(crate) fn should_report_error(error: &Error) -> bool {
    error.downcast_ref::<OutputAlreadyReported>().is_none()
}

// ── JSON types ───────────────────────────────────────────

#[derive(Serialize)]
pub struct JsonError {
    pub ok: bool,
    pub error: String,
}

// ── Conversion helpers ───────────────────────────────────

/// Relative reset for quota tables, using one snapshot of local now per table.
pub(crate) fn format_reset_relative(ts: Option<i64>, now: i64) -> String {
    let Some(ts) = ts.filter(|ts| Local.timestamp_opt(*ts, 0).single().is_some()) else {
        return "--".into();
    };
    if ts < now {
        return "now".into();
    }
    let secs = ts.saturating_sub(now);
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}d{}h", secs / 86400, secs % 86400 / 3600)
    }
}

/// Shared timestamp formatter: "2h30m (14:30)" or "1d12h (03-27 14:30)"
pub fn format_reset_time(ts: i64) -> String {
    let now = Local::now();
    let dt: DateTime<Local> = match Local.timestamp_opt(ts, 0).single() {
        Some(d) => d,
        None => return "--".into(),
    };
    if dt <= now {
        return "expired".into();
    }
    let secs = (dt - now).num_seconds().max(0) as u64;
    let relative = if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
    };
    let local_fmt = if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%m-%d %H:%M").to_string()
    };
    format!("{relative} ({local_fmt})")
}

/// Short reset time for table columns: "14:30" or "03-27 14:30"
pub fn format_reset_short(ts: i64) -> String {
    let now = Local::now();
    let dt: DateTime<Local> = match Local.timestamp_opt(ts, 0).single() {
        Some(d) => d,
        None => return "--".into(),
    };
    if dt <= now {
        return "reset".into();
    }
    if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%m-%d %H:%M").to_string()
    }
}

/// Format a timestamp as local time: "HH:MM" (today) or "MM-DD HH:MM" (other days).
pub fn format_local_time(ts: i64) -> String {
    let now = Local::now();
    let dt: DateTime<Local> = match Local.timestamp_opt(ts, 0).single() {
        Some(d) => d,
        None => return "--".into(),
    };
    if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%m-%d %H:%M").to_string()
    }
}

/// Full local timestamp for detail views. The UTC offset keeps the value
/// unambiguous when screenshots or logs cross time zones.
pub fn format_local_timestamp(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M %:z").to_string())
        .unwrap_or_else(|| "--".into())
}

/// Format a token expiry with an explicit state so past JWT `exp` values are
/// never presented as a future expiration.
pub fn format_token_expiry(ts: i64) -> String {
    let Some(dt) = Local.timestamp_opt(ts, 0).single() else {
        return "not reported".into();
    };
    let timestamp = dt.format("%Y-%m-%d %H:%M %:z");
    if dt <= Local::now() {
        format!("expired {timestamp}")
    } else {
        format!("expires {timestamp}")
    }
}

// ── Output ───────────────────────────────────────────────

static JSON_PRETTY: OnceLock<bool> = OnceLock::new();

// 0 = Stdout, 1 = Stderr, 2 = Silent
static MESSAGE_MODE: AtomicU8 = AtomicU8::new(0);

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum MessageMode {
    Stdout = 0,
    Stderr = 1,
    Silent = 2,
}

/// Set JSON output mode. Call once at startup.
pub fn set_json_pretty(pretty: bool) {
    let _ = JSON_PRETTY.set(pretty);
}

pub fn set_message_mode(mode: MessageMode) {
    MESSAGE_MODE.store(mode as u8, Ordering::Relaxed);
}

fn is_pretty() -> bool {
    *JSON_PRETTY.get().unwrap_or(&false)
}

fn message_mode() -> MessageMode {
    match MESSAGE_MODE.load(Ordering::Relaxed) {
        1 => MessageMode::Stderr,
        2 => MessageMode::Silent,
        _ => MessageMode::Stdout,
    }
}

fn serialize<T: serde::Serialize>(val: &T) -> String {
    if is_pretty() {
        serde_json::to_string_pretty(val)
    } else {
        serde_json::to_string(val)
    }
    .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

pub fn print_json<T: serde::Serialize>(val: &T) {
    println!("{}", serialize(val));
}

pub fn print_error(msg: &str) {
    let e = JsonError {
        ok: false,
        error: msg.to_string(),
    };
    println!("{}", serialize(&e));
}

pub fn user_println(msg: &str) {
    match message_mode() {
        MessageMode::Stdout => println!("{msg}"),
        MessageMode::Stderr => eprintln!("{msg}"),
        MessageMode::Silent => {}
    }
}

pub struct ProgressReporter {
    enabled: bool,
    label: String,
    total: usize,
    last_width: usize,
}

impl ProgressReporter {
    pub fn new(label: &str, total: usize) -> Self {
        let enabled = progress_enabled() && total > 0;
        let mut reporter = Self {
            enabled,
            label: label.to_string(),
            total,
            last_width: 0,
        };
        if reporter.enabled {
            reporter.advance(0);
        }
        reporter
    }

    pub fn advance(&mut self, completed: usize) {
        if !self.enabled {
            return;
        }

        let line = render_progress_line(&self.label, completed.min(self.total), self.total);
        self.last_width = line.chars().count();
        eprint!("\r{line}");
        let _ = io::stderr().flush();
    }

    pub fn finish(&mut self) {
        if !self.enabled {
            return;
        }

        eprint!("\r{}\r", " ".repeat(self.last_width.max(1)));
        let _ = io::stderr().flush();
        self.enabled = false;
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        self.finish();
    }
}

pub fn render_progress_line(label: &str, completed: usize, total: usize) -> String {
    let total = total.max(1);
    let completed = completed.min(total);
    let width = 24usize;
    let filled = completed.saturating_mul(width) / total;
    let bar = format!(
        "{}{}",
        "#".repeat(filled),
        "-".repeat(width.saturating_sub(filled))
    );
    format!("{label} [{bar}] {completed}/{total}")
}

fn progress_enabled() -> bool {
    if matches!(message_mode(), MessageMode::Silent) {
        return false;
    }
    if std::env::var("CS_PROGRESS_FORCE").ok().as_deref() == Some("1") {
        return true;
    }
    io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_reset_relative_boundaries() {
        let now = 1_800_000_000;
        for (seconds, expected) in [
            (0, "0m"), (59 * 60, "59m"), (2 * 3600 + 13 * 60, "2h13m"),
            (86400, "1d0h"), (5 * 86400 + 4 * 3600, "5d4h"), (-1, "now"),
        ] {
            assert_eq!(format_reset_relative(Some(now + seconds), now), expected);
        }
        assert_eq!(format_reset_relative(None, now), "--");
        assert_eq!(format_reset_relative(Some(i64::MAX), now), "--");
    }

    #[test]
    fn already_reported_errors_are_not_reported_twice() {
        assert!(!should_report_error(&OutputAlreadyReported.into()));
        assert!(should_report_error(&anyhow::anyhow!("new failure")));
    }

    #[test]
    fn local_timestamp_includes_date_time_and_system_offset() {
        let rendered = format_local_timestamp(1_783_857_600);
        let expected = Local
            .timestamp_opt(1_783_857_600, 0)
            .single()
            .unwrap()
            .format("%Y-%m-%d %H:%M %:z")
            .to_string();

        assert_eq!(rendered, expected);
        assert!(!rendered.ends_with('Z'));
    }

    #[test]
    fn token_expiry_marks_past_timestamps_as_expired() {
        let text = format_token_expiry(crate::auth::now_unix_secs() - 60);
        assert!(text.starts_with("expired "));
    }
}
