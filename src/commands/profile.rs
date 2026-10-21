use anyhow::{Context, Result};
use serde_json::{Value, json};
use crate::{agy_store, agy_usage, cache, color, output, profile, usage};

pub(crate) fn switch_alias(alias: &str) -> Result<agy_store::SwitchOutcome> {
    let result = profile::switch_profile(alias)?;
    if let Err(error) = cache::set_last_used(alias) {
        tracing::warn!("could not record last use: {error}");
    }
    Ok(result)
}

pub(crate) fn announce_switch(alias: &str, result: &agy_store::SwitchOutcome) -> Result<()> {
    let store = agy_usage::store()?;
    let identity = agy_store::identity_of(&store.profile_blob(alias)?)?;
    let email = identity.email.as_deref().unwrap_or("unknown email");
    match result {
        agy_store::SwitchOutcome::Switched { .. } => {
            output::user_println(&format!("Switched to {alias} ({email})."));
            output::user_println(&color::dim("Running agy sessions and the Antigravity app may keep the previous account until restarted."));
        }
        agy_store::SwitchOutcome::AlreadyActive => output::user_println(&format!("{alias} ({email}) is already active.")),
    }
    Ok(())
}

fn resolve_alias(requested: &str) -> Result<String> {
    let profiles = agy_usage::profiles()?;
    if !profiles.iter().any(|p| p.alias == requested) {
        if let Some(p) = requested.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|n| profiles.get(n)) {
            return Ok(p.alias.clone());
        }
    }
    profile::validate_alias(requested)?;
    Ok(requested.to_owned())
}

pub(crate) async fn use_cmd(alias: Option<&str>, json_mode: bool) -> Result<()> {
    let alias = match alias {
        Some(alias) => resolve_alias(alias)?,
        None => select_best_profile(json_mode).await?.alias,
    };
    let result = switch_alias(&alias)?;
    if json_mode {
        output::print_json(&json!({"ok":true,"alias":alias,"action":match result {
            agy_store::SwitchOutcome::AlreadyActive => "already_active", _ => "switched"
        }}));
    }
    announce_switch(&alias, &result)
}

pub(crate) struct SelectOutcome { pub(crate) alias: String }

pub(crate) async fn select_best_profile(_json: bool) -> Result<SelectOutcome> {
    let ranked = agy_usage::rank_pool(false).await?;
    let (candidate, _, _) = ranked.into_iter().next().context("no usable saved profiles; run `paper-agy-switch login` first")?;
    Ok(SelectOutcome { alias: candidate.alias })
}

fn usage_json(value: &usage::UsageInfo) -> Value {
    let gemini = usage::gemini_pool(value);
    json!({"primary":value.primary,"secondary":value.secondary,"gemini":{
        "primary":gemini.and_then(|p| p.primary.as_ref()),
        "secondary":gemini.and_then(|p| p.secondary.as_ref())
    }})
}

fn window_text(window: Option<&usage::WindowUsage>) -> String {
    window.map(|w| {
        let used = w.used_percent.map(|v| color::usage_pct(&format!("{v:.0}%"), v)).unwrap_or_else(|| "--".into());
        let reset = w.resets_at.and_then(|at| chrono::DateTime::from_timestamp(at, 0))
            .map(|at| at.to_rfc3339()).unwrap_or_else(|| "--".into());
        format!("{used} (resets {reset})")
    }).unwrap_or_else(|| "--".into())
}

pub(crate) async fn list_cmd(force: bool, json_mode: bool) -> Result<()> {
    let store = agy_usage::store()?;
    let active = store.active_alias()?;
    let mut rows = Vec::new();
    if !json_mode { println!("  Alias  Email  Tier  Claude/GPT 5h | weekly  Gemini 5h | weekly"); }
    let profiles = agy_usage::profiles()?;
    let mut progress = (!json_mode).then(|| output::ProgressReporter::new("Refreshing usage", profiles.len()));
    for (index, p) in profiles.into_iter().enumerate() {
        let result = agy_usage::fetch_alias(&p.alias, force).await;
        if let Some(progress) = &mut progress { progress.advance(index + 1); }
        let is_active = active.as_deref() == Some(&p.alias);
        let (value, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let tier = value.as_ref().and_then(|u| u.plan_type.as_deref());
        if json_mode {
            rows.push(json!({"alias":p.alias,"email":p.email,"active":is_active,"tier":tier,
                "usage":value.as_ref().map(usage_json),"error":error}));
        } else {
            print!("{} {}  {}  {}", if is_active { color::active("*") } else { " ".into() }, color::bold(&p.alias),
                p.email.as_deref().unwrap_or("unknown email"), tier.unwrap_or("--"));
            if let Some(value) = &value {
                let gemini = usage::gemini_pool(value);
                print!("  {} | {}  {} | {}", window_text(value.primary.as_ref()), window_text(value.secondary.as_ref()),
                    window_text(gemini.and_then(|p| p.primary.as_ref())), window_text(gemini.and_then(|p| p.secondary.as_ref())));
            }
            if let Some(error) = error { print!("  {error}"); }
            println!();
        }
    }
    if json_mode { output::print_json(&rows); }
    Ok(())
}

pub(crate) fn rename_cmd(old: &str, new: &str, json_mode: bool) -> Result<()> {
    profile::rename_profile(old, new)?;
    if json_mode { output::print_json(&json!({"ok":true,"alias":new,"action":"renamed"})); }
    Ok(())
}

pub(crate) fn delete_cmd(alias: &str, yes: bool, json_mode: bool) -> Result<()> {
    use std::io::IsTerminal;
    let alias = resolve_alias(alias)?;
    if agy_usage::store()?.active_alias()?.as_deref() == Some(&alias) {
        anyhow::bail!("cannot delete the active profile '{alias}'");
    }
    if !yes {
        if json_mode || !std::io::stdin().is_terminal() { anyhow::bail!("confirmation required; rerun with --yes"); }
        if !super::render::confirm_default_no(&format!("Delete profile '{alias}'? It will remain recoverable. [y/N] ")) { return Ok(()); }
    }
    profile::cmd_delete(&alias)?;
    if json_mode { output::print_json(&json!({"ok":true,"alias":alias,"action":"deleted"})); }
    Ok(())
}

pub(crate) fn restore_cmd(alias: Option<&str>, as_alias: Option<&str>, json_mode: bool) -> Result<()> {
    if let Some(alias) = alias {
        let restored = profile::cmd_restore(alias, as_alias)?;
        if json_mode { output::print_json(&json!({"ok":true,"alias":restored,"action":"restored"})); }
        else { println!("Restored profile: {restored}"); }
    } else {
        let deleted = profile::list_deleted()?;
        if json_mode { output::print_json(&json!({"deleted":deleted.iter().map(|(a,_)| a).collect::<Vec<_>>()})); }
        else { for (alias, archive) in deleted { println!("{alias}  {archive}"); } }
    }
    Ok(())
}
