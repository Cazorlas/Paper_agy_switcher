use anyhow::{Context, Result};
use crate::{agy_store, agy_usage, output, profile};

pub(crate) fn login_cmd(alias: Option<&str>, json: bool) -> Result<()> {
    let store = agy_usage::store()?;
    let live = store.read_live()?.context(
        "not signed in to Antigravity - sign in with `agy` first, then run `paper-agy-switch login <alias>`"
    )?;
    let identity = agy_store::identity_of(&live)?;
    let derived = identity.email.as_deref().unwrap_or("account").split('@').next().unwrap_or("account")
        .chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' })
        .take(64).collect::<String>();
    let derived = if derived.is_empty() || derived == "." || derived == ".." { "account" } else { &derived };
    let alias = alias.unwrap_or(derived);
    let _lock = profile::lock_live_auth()?;
    let action = store.save_current(alias)?;
    let action = match action { agy_store::SaveAction::Created => "created", agy_store::SaveAction::Updated => "updated" };
    if json {
        output::print_json(&serde_json::json!({"ok":true,"alias":alias,"email":identity.email,"action":action}));
    } else {
        println!("Saved {alias} ({}).", identity.email.as_deref().unwrap_or("unknown email"));
    }
    Ok(())
}
