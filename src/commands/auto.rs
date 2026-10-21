//! `auto`: watch the active account's quota and swap to a better one before
//! Antigravity hits its usage limit.

use crate::{auth, cache, agy_usage, color, config, profile, usage};
use anyhow::Result;
use std::time::{Duration, Instant};

/// Exit codes for `auto --once`, so cron and scripts can branch on the result.
pub(crate) const EXIT_SWITCHED: i32 = 0;
pub(crate) const EXIT_NOTHING_TO_DO: i32 = 2;
pub(crate) const EXIT_BLOCKED: i32 = 3;

#[derive(Debug, Clone)]
pub(crate) struct AutoOptions {
    /// Switch away once the active account's 5h or 7d window reaches this percent.
    pub threshold: f64,
    /// A target must be at least this many points below the active account.
    pub margin: f64,
    pub interval: Duration,
    pub cooldown: Duration,
    pub once: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// Active account is under the threshold.
    Stay { pressure: f64 },
    /// Switch to `alias`.
    Switch {
        alias: String,
        from_pressure: f64,
        to_pressure: f64,
    },
    /// Active account is over the threshold and no account is a viable target.
    Blocked { pressure: f64 },
}

/// The busier of the two quota windows, as a used percent.
pub(crate) fn pressure(c: &usage::Candidate) -> f64 {
    let five = if c.has_5h_data { c.used_5h } else { 0.0 };
    let seven = if c.has_7d_data { c.used_7d } else { 0.0 };
    five.max(seven)
}

/// Pure decision. `ranked` is the scored pool, best first (see
/// `score_profile_candidates`), and may include the active account.
pub(crate) fn decide(
    current: &str,
    ranked: &[(usage::Candidate, f64)],
    limited_now: bool,
    opts: &AutoOptions,
    safety_7d: f64,
) -> Decision {
    let cur_pressure = ranked
        .iter()
        .find(|(c, _)| c.alias == current)
        .map(|(c, _)| pressure(c))
        .unwrap_or(0.0);
    let cur_pressure = if limited_now {
        cur_pressure.max(100.0)
    } else {
        cur_pressure
    };
    if cur_pressure < opts.threshold {
        return Decision::Stay {
            pressure: cur_pressure,
        };
    }
    let target = ranked.iter().find(|(c, _)| {
        c.alias != current
            && usage::is_candidate_eligible(c, safety_7d)
            && pressure(c) < opts.threshold
            && pressure(c) + opts.margin <= cur_pressure
    });
    match target {
        Some((c, _)) => Decision::Switch {
            alias: c.alias.clone(),
            from_pressure: cur_pressure,
            to_pressure: pressure(c),
        },
        None => Decision::Blocked {
            pressure: cur_pressure,
        },
    }
}

fn emit(opts: &AutoOptions, event: &str, detail: serde_json::Value, text: String) {
    if opts.json {
        let mut obj = serde_json::json!({ "event": event, "at": auth::now_unix_secs() });
        if let (Some(o), Some(d)) = (obj.as_object_mut(), detail.as_object()) {
            o.extend(d.clone());
        }
        println!("{obj}");
    } else {
        println!("{text}");
    }
}

/// Fetch usage for every profile (forced: a switch decision must not run on a
/// stale cache) and rank them.
pub(crate) async fn tick(opts: &AutoOptions, current: &str) -> Result<Decision> {
    let pool = agy_usage::rank_pool(true).await?;
    if !pool.iter().any(|(candidate, _, _)| candidate.alias == current) {
        anyhow::bail!("usage check for active account failed");
    }
    let ranked: Vec<_> = pool.into_iter().map(|(c, _, s)| (c, s)).collect();
    Ok(decide(current, &ranked, false, opts, config::get().use_cfg.safety_margin_7d))
}

fn apply(current: &str, alias: &str) -> Result<bool> {
    // Compare-and-swap: bail if another process switched the live account.
    let _lock = profile::lock_live_auth()?;
    let store = agy_usage::store()?;
    if store.active_alias()?.as_deref() != Some(current) {
        return Ok(false);
    }
    let result = store.switch_to(alias)?;
    if let Err(error) = cache::set_last_used(alias) { tracing::warn!("could not record last use: {error}"); }
    super::profile::announce_switch(alias, &result)?;
    tracing::info!(
        action = "switch",
        alias,
        selection = "auto-swap",
        "account switched"
    );
    Ok(true)
}

pub(crate) async fn auto_cmd(opts: AutoOptions) -> Result<()> {
    if !opts.once && !opts.json {
        println!(
            "{}",
            color::success(&format!(
                "auto is running: switches accounts at {:.0}% (5h or 7d), checks every {}s.",
                opts.threshold,
                opts.interval.as_secs()
            ))
        );
        println!(
            "{}",
            color::dim("Leave this window open (you can minimize it). Stop with Ctrl+C or by closing the window.")
        );
    }
    let mut last_switch: Option<Instant> = None;
    let mut last_blocked = false;

    loop {
        let Some(current) = agy_usage::store()?.active_alias()? else {
            anyhow::bail!(
                "the live Antigravity account is not a saved profile; run `paper-agy-switch login` first"
            );
        };
        let in_cooldown = last_switch.is_some_and(|t| t.elapsed() < opts.cooldown);

        let code = match tick(&opts, &current).await {
            Err(e) => {
                // Fail safe: keep the current account and retry next tick.
                emit(
                    &opts,
                    "error",
                    serde_json::json!({ "message": format!("{e:#}") }),
                    color::error(&format!("auto: {e:#}")),
                );
                if opts.once {
                    return Err(e);
                }
                None
            }
            Ok(Decision::Stay { pressure }) => {
                last_blocked = false;
                emit(
                    &opts,
                    "ok",
                    serde_json::json!({ "alias": current, "pressure": pressure }),
                    color::dim(&format!("auto: '{current}' at {pressure:.0}%, staying")),
                );
                Some(EXIT_NOTHING_TO_DO)
            }
            Ok(Decision::Blocked { pressure }) => {
                if !last_blocked || opts.once {
                    emit(
                        &opts,
                        "blocked",
                        serde_json::json!({ "alias": current, "pressure": pressure }),
                        color::warn(&format!(
                            "auto: '{current}' at {pressure:.0}% and no account is a viable target"
                        )),
                    );
                }
                last_blocked = true;
                Some(EXIT_BLOCKED)
            }
            Ok(Decision::Switch {
                alias,
                from_pressure,
                to_pressure,
            }) => {
                last_blocked = false;
                let detail = serde_json::json!({
                    "from": current, "to": alias,
                    "from_pressure": from_pressure, "to_pressure": to_pressure,
                    "dry_run": opts.dry_run,
                });
                let summary = format!(
                    "'{current}' ({from_pressure:.0}%) -> '{alias}' ({to_pressure:.0}%)"
                );
                if in_cooldown && !opts.once {
                    emit(
                        &opts,
                        "cooldown",
                        detail,
                        color::dim(&format!("auto: want {summary} but in cooldown")),
                    );
                    None
                } else if opts.dry_run {
                    emit(
                        &opts,
                        "would_switch",
                        detail,
                        format!("auto: would switch {summary}"),
                    );
                    Some(EXIT_NOTHING_TO_DO)
                } else if apply(&current, &alias)? {
                    last_switch = Some(Instant::now());
                    emit(
                        &opts,
                        "switched",
                        detail,
                        color::success(&format!("auto: switched {summary}")),
                    );
                    Some(EXIT_SWITCHED)
                } else {
                    emit(
                        &opts,
                        "raced",
                        detail,
                        color::dim("auto: active profile changed under us; re-checking"),
                    );
                    None
                }
            }
        };

        if opts.once {
            std::process::exit(code.unwrap_or(1));
        }

        // Exhausted pool: back off to a slower cadence instead of hammering the API.
        let wait = if last_blocked {
            opts.interval.max(Duration::from_secs(600))
        } else {
            opts.interval
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> AutoOptions {
        AutoOptions {
            threshold: 90.0,
            margin: 10.0,
            interval: Duration::from_secs(60),
            cooldown: Duration::from_secs(300),
            once: true,
            dry_run: false,
            json: false,
        }
    }

    fn cand(alias: &str, used_5h: f64, used_7d: f64) -> usage::Candidate {
        usage::Candidate {
            alias: alias.into(),
            used_5h,
            resets_at_5h: None,
            used_7d,
            resets_at_7d: None,
            has_5h_data: true,
            has_7d_data: true,
            is_team: false,
            is_free: false,
            last_used: 0,
            now: 0,
            pool_size: 2,
            pool_exhausted: 0,
            team_priority: false,
        }
    }

    #[test]
    fn stays_under_threshold() {
        let r = vec![(cand("a", 50.0, 10.0), 1.0), (cand("b", 0.0, 0.0), 2.0)];
        assert!(matches!(
            decide("a", &r, false, &opts(), 20.0),
            Decision::Stay { .. }
        ));
    }

    #[test]
    fn switches_to_best_when_over_threshold() {
        let r = vec![(cand("b", 10.0, 10.0), 2.0), (cand("a", 95.0, 10.0), 1.0)];
        match decide("a", &r, false, &opts(), 20.0) {
            Decision::Switch { alias, .. } => assert_eq!(alias, "b"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn blocked_when_every_other_account_is_also_hot() {
        let r = vec![(cand("a", 95.0, 10.0), 1.0), (cand("b", 92.0, 10.0), 2.0)];
        assert!(matches!(
            decide("a", &r, false, &opts(), 20.0),
            Decision::Blocked { .. }
        ));
    }

    #[test]
    fn hysteresis_blocks_marginal_candidates() {
        let mut o = opts();
        o.threshold = 80.0;
        let r = vec![(cand("a", 82.0, 0.0), 1.0), (cand("b", 75.0, 0.0), 2.0)];
        assert!(matches!(
            decide("a", &r, false, &o, 20.0),
            Decision::Blocked { .. }
        ));
    }

    #[test]
    fn seven_day_window_also_triggers() {
        let r = vec![(cand("a", 5.0, 93.0), 1.0), (cand("b", 5.0, 5.0), 2.0)];
        assert!(matches!(
            decide("a", &r, false, &opts(), 20.0),
            Decision::Switch { .. }
        ));
    }
}
