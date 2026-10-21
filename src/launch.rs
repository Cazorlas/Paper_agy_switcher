use crate::signals::{ShutdownListener, ShutdownSignal};
use anyhow::{Context, Result};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TuiLaunchOutcome {
    Exited(i32),
    Shutdown {
        signal: ShutdownSignal,
        cleanup_error: Option<String>,
    },
}

async fn wait_for_child_or_shutdown(
    child: &mut std::process::Child,
    shutdown: &mut ShutdownListener,
) -> std::io::Result<Result<std::process::ExitStatus, ShutdownSignal>> {
    loop {
        tokio::select! {
            signal = shutdown.recv_signal() => return Ok(Err(signal)),
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if let Some(status) = child.try_wait()? {
                    return Ok(Ok(status));
                }
            }
        }
    }
}

/// Launch Antigravity for one alias from the TUI. Returns Antigravity's exit
/// code instead of terminating the paper-agy-switch process on failure.
pub(crate) async fn launch_for_tui(
    alias: &str,
    extra_args: Vec<String>,
    shutdown: &mut ShutdownListener,
) -> Result<TuiLaunchOutcome> {
    launch_interactive(Some(alias), extra_args, false, Some(shutdown)).await
}

pub(crate) async fn launch_cmd(
    alias: Option<&str>,
    args: Vec<String>,
    json: bool,
) -> Result<()> {
    finish_launch_cli(launch_interactive(alias, args, json, None).await?)
}

fn finish_launch_cli(outcome: TuiLaunchOutcome) -> Result<()> {
    let exit_code = match outcome {
        TuiLaunchOutcome::Exited(code) => code,
        TuiLaunchOutcome::Shutdown { signal, .. } => signal.exit_code(),
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Switch to the alias (or the best account), then run Antigravity with the
/// passthrough args. The binary is resolved first, so a missing `agy`
/// leaves the live login alone.
async fn launch_interactive(
    alias: Option<&str>,
    args: Vec<String>,
    json: bool,
    tui_shutdown: Option<&mut ShutdownListener>,
) -> Result<TuiLaunchOutcome> {
    let agy = ensure_agy_available()?;

    let mut owned_shutdown;
    let interrupt = match tui_shutdown {
        Some(shutdown) => shutdown,
        None => {
            owned_shutdown = ShutdownListener::new().context("registering shutdown handlers")?;
            &mut owned_shutdown
        }
    };

    let target_alias = match alias {
        Some(alias) => alias.to_string(),
        None => crate::commands::profile::select_best_profile(json).await?.alias,
    };
    let outcome = tokio::task::spawn_blocking({
        let target_alias = target_alias.clone();
        move || crate::commands::profile::switch_alias(&target_alias)
    })
    .await
    .context("switch task panicked")??;
    crate::commands::profile::announce_switch(&target_alias, &outcome)?;

    let mut child = spawn_agy(&agy, &args).context("Failed to start agy")?;
    let status = match wait_for_child_or_shutdown(&mut child, interrupt)
        .await
        .context("waiting for agy")?
    {
        Ok(status) => status,
        Err(signal) => {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(TuiLaunchOutcome::Shutdown { signal, cleanup_error: None });
        }
    };
    Ok(TuiLaunchOutcome::Exited(child_exit_code(&status)))
}

fn spawn_agy(
    command: &std::path::Path,
    args: &[String],
) -> std::io::Result<std::process::Child> {
    agy_command(command).args(args).spawn()
}

pub(crate) fn agy_command(command: &std::path::Path) -> std::process::Command {
    let is_script = cfg!(windows) && command.extension().and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if is_script {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/D").arg("/C").arg(command);
        cmd
    } else { std::process::Command::new(command) }
}

/// Resolve the agy CLI from PATH.
fn ensure_agy_available() -> Result<std::path::PathBuf> {
    command_on_path("agy").ok_or_else(|| anyhow::anyhow!("agy not found on PATH; install Antigravity first"))
}

pub(crate) fn command_on_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let candidates = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';').filter(|ext| !ext.is_empty()).map(|ext| format!("{name}{}", ext.to_lowercase()))
            .chain(std::iter::once(name.to_owned())).collect::<Vec<_>>()
    } else { vec![name.to_owned()] };
    for dir in std::env::split_paths(&paths) {
        for file in &candidates {
            let candidate = dir.join(file);
            if candidate.is_file() {
                return if candidate.is_absolute() {
                    Some(candidate)
                } else {
                    std::env::current_dir().ok().map(|cwd| cwd.join(candidate))
                };
            }
        }
    }
    None
}

/// The child's exit code, mapping a Unix signal death to `128 + signal`.
fn child_exit_code(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        status.code().unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            status.signal().map(|s| 128 + s).unwrap_or(1)
        })
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(1)
    }
}
