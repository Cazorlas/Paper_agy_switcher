// Claude's "reset your session limit" offer (`juniper_tide` in the usage reply):
// parsed by parse_usage, kept in the cache, shown by `list` as a Reset column.

use agy_switch::claude_api::{SessionReset, parse_usage};
use serde_json::{Value, json};

fn available_block() -> Value {
    json!({
        "eligible": true,
        "available": true,
        "resets_per_week": 2,
        "event_props": {"billing_period": "monthly"}
    })
}

fn usage_body(juniper_tide: Value) -> Value {
    json!({
        "five_hour": {"utilization": 10.0, "resets_at": "2099-01-01T00:00:00Z"},
        "juniper_tide": juniper_tide
    })
}

// ── S1-S3: parse_usage ───────────────────────────────────

/// S1
#[test]
fn an_available_offer_is_parsed() {
    let usage = parse_usage(&usage_body(available_block())).expect("usable usage");

    assert_eq!(
        usage.session_reset,
        Some(SessionReset {
            eligible: true,
            ineligible_reason: None,
            available: true,
            next_available_at: None,
            resets_per_week: 2,
            billing_period: Some("monthly".to_owned()),
        })
    );
}

/// S2
#[test]
fn a_used_offer_carries_its_next_time_and_defaults_to_one_reset_a_week() {
    let usage = parse_usage(&usage_body(json!({
        "eligible": true,
        "available": false,
        "next_available_at": "2026-10-12T09:00:00Z"
    })))
    .expect("usable usage");

    let reset = usage.session_reset.expect("session_reset");
    assert!(reset.eligible);
    assert!(!reset.available);
    assert_eq!(reset.next_available_at.as_deref(), Some("2026-10-12T09:00:00Z"));
    assert_eq!(reset.resets_per_week, 1);
}

/// S3
#[test]
fn a_null_or_malformed_offer_is_ignored() {
    for block in [Value::Null, json!("garbage")] {
        let usage = parse_usage(&usage_body(block.clone())).expect("usable usage");
        assert_eq!(usage.session_reset, None, "juniper_tide: {block}");
        assert_eq!(usage.five_hour.expect("five_hour").pct, 10.0, "juniper_tide: {block}");
    }
    let without = parse_usage(&json!({
        "five_hour": {"utilization": 10.0, "resets_at": "2099-01-01T00:00:00Z"}
    }))
    .expect("usable usage");
    assert_eq!(without.session_reset, None);
    assert!(parse_usage(&json!({})).is_none(), "an empty reply stays unusable");
    // Ignoring a bad block must not mean ignoring every block.
    let valid = parse_usage(&usage_body(available_block())).expect("usable usage");
    assert!(valid.session_reset.is_some(), "a well-formed block is kept");
}
