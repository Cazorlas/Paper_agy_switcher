use crate::output::print_json;
use crate::{auth, agy_store, agy_usage, color};
use anyhow::{Context, Result};

// ── open ─────────────────────────────────────────────────

pub(crate) fn open_cmd() -> Result<()> {
    let dir = auth::app_home()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer.exe")
        .arg(dir.as_os_str())
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(&dir).spawn();
    match result {
        Ok(_) => println!("Opened: {}", dir.display()),
        Err(e) => println!(
            "{}",
            color::error(&format!(
                "Could not open file manager: {e}\nPath: {}",
                dir.display()
            ))
        ),
    }
    Ok(())
}

// ── doctor ────────────────────────────────────────────────

pub(crate) fn doctor_cmd(json: bool) -> Result<()> {
    let backend = agy_store::backend_from_env()?;
    let description = backend.describe();
    let identity = backend.read()?.map(|blob| agy_store::identity_of(&blob)).transpose()?;
    let email = identity.and_then(|i| i.email);
    let path = crate::launch::command_on_path("agy");
    let version = path.as_ref().and_then(|path| {
        let output = crate::launch::agy_command(path).arg("--version").output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    });
    let running = crate::running_agy::list()?;
    let count = agy_usage::profiles()?.len();
    let pool = crate::config::get().use_cfg.pool;
    if json {
        print_json(&serde_json::json!({"credential_store":description,"live_email":email,
            "agy_path":path,"agy_version":version,"running_agy":running,"profiles":count,"pool":pool}));
    } else {
        println!("Credential store: {description}");
        println!("Live login: {}", email.as_deref().unwrap_or("not signed in"));
        match path {
            Some(path) => println!("agy found on PATH: {} ({})", path.display(), version.as_deref().unwrap_or("version unavailable")),
            None => println!("agy not found on PATH"),
        }
        println!("Running agy processes:\n{}", crate::running_agy::format_processes(&running));
        println!("Profiles: {count}");
        println!("Scoring pool: {}", serde_json::to_value(pool)?.as_str().unwrap_or("claude"));
    }
    Ok(())
}
