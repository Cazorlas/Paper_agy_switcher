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
    crate::running_agy::warn(&crate::running_agy::list()?);
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

pub(crate) async fn use_cmd(alias: Option<&str>, json_mode: bool, restart_agy: bool) -> Result<()> {
    let alias = match alias {
        Some(alias) => resolve_alias(alias)?,
        None => select_best_profile(json_mode).await?.alias,
    };
    let result = switch_alias(&alias)?;
    let running = crate::running_agy::list()?;
    let stopped = if restart_agy { crate::running_agy::restart(&running)? } else { Vec::new() };
    if json_mode {
        output::print_json(&json!({"ok":true,"alias":alias,"running_agy":running.iter().map(|p| json!({"pid":p.pid,"parent":p.parent})).collect::<Vec<_>>(),"stopped_agy":stopped,"action":match result {
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

fn plan_label(tier: &str) -> &str {
    match tier {
        "g1-pro-tier" => "Pro",
        "g1-ultra-tier" => "Ultra",
        "free-tier" => "Free",
        other => other,
    }
}

fn window_text(window: Option<&usage::WindowUsage>, now: i64) -> String {
    window.map(|w| {
        let used = if w.disabled { "off".into() } else {
            w.used_percent.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "--".into())
        };
        let reset = output::format_reset_relative(w.resets_at, now);
        if reset == "--" { format!("{used} --") } else { format!("{used} ↻{reset}") }
    }).unwrap_or_else(|| "--".into())
}

struct ListRow {
    cells: [String; 10],
    percentages: [Option<f64>; 4],
}

fn list_table(rows: &[ListRow]) -> String {
    let headers = ["", "#", "Alias", "Email", "Plan", "Claude/GPT 5h", "Claude/GPT week", "Gemini 5h", "Gemini week", "Status"];
    let widths: Vec<_> = (0..headers.len()).map(|column| {
        std::iter::once(headers[column]).chain(rows.iter().map(|row| row.cells[column].as_str()))
            .map(|cell| ratatui::text::Line::raw(cell).width()).max().unwrap_or(0)
    }).collect();
    let mut lines = vec![headers.iter().enumerate().map(|(i, cell)| {
        super::render::padded_cell(cell, widths[i], color::bold)
    }).collect::<Vec<_>>().join("  ").trim_end().to_string()];
    for row in rows {
        let line = row.cells.iter().enumerate().map(|(i, cell)| {
            super::render::padded_cell(cell, widths[i], |padded| match i {
                0 => color::active(padded),
                2 => color::bold(padded),
                5..=8 => row.percentages[i - 5].map(|pct| color::usage_pct(padded, pct))
                    .unwrap_or_else(|| color::dim(padded)),
                9 if !cell.is_empty() => color::error(padded),
                _ => padded.to_string(),
            })
        }).collect::<Vec<_>>().join("  ");
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

pub(crate) async fn list_cmd(force: bool, json_mode: bool) -> Result<()> {
    let store = agy_usage::store()?;
    let active = store.active_alias()?;
    let source = crate::config::get().quota.source;
    let mut rows = Vec::new();
    let mut table_rows = Vec::new();
    let now = chrono::Local::now().timestamp();
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
            rows.push(json!({"quota_source":source,"alias":p.alias,"email":p.email,"active":is_active,"tier":tier,
                "usage":value.as_ref().map(usage_json),"error":error}));
        } else {
            let gemini = value.as_ref().and_then(usage::gemini_pool);
            let windows = [value.as_ref().and_then(|u| u.primary.as_ref()), value.as_ref().and_then(|u| u.secondary.as_ref()),
                gemini.and_then(|p| p.primary.as_ref()), gemini.and_then(|p| p.secondary.as_ref())];
            table_rows.push(ListRow {
                cells: [if is_active { "*".into() } else { "".into() }, (index + 1).to_string(), p.alias,
                    p.email.unwrap_or_else(|| "unknown email".into()), plan_label(tier.unwrap_or("--")).into(),
                    window_text(windows[0], now), window_text(windows[1], now),
                    window_text(windows[2], now), window_text(windows[3], now), error.unwrap_or_default()],
                percentages: windows.map(|w| w.filter(|w| !w.disabled).and_then(|w| w.used_percent)),
            });
        }
    }
    if let Some(progress) = &mut progress { progress.finish(); }
    if json_mode { output::print_json(&rows); } else { println!("quota: {}\n{}", source.label(), list_table(&table_rows)); }
    Ok(())
}

#[cfg(test)]
mod list_tests {
    use super::*;

    #[test]
    fn disabled_window_keeps_reset_without_percentage() {
        let window = usage::WindowUsage {
            used_percent: Some(53.1232), disabled: true, resets_at: Some(1_000_000 + 4 * 3600 + 16 * 60),
            ..Default::default()
        };
        assert_eq!(window_text(Some(&window), 1_000_000), "off ↻4h16m");
    }

    #[test]
    fn plan_labels_keep_unknown_ids() {
        for (tier, label) in [("g1-pro-tier", "Pro"), ("g1-ultra-tier", "Ultra"), ("free-tier", "Free"), ("future-tier", "future-tier"), ("--", "--")] {
            assert_eq!(plan_label(tier), label);
        }
    }
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
