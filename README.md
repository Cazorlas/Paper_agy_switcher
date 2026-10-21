# paper-agy-switch

**Multi-account switcher for Google Antigravity, with a quota dashboard for the agy CLI and Antigravity desktop app.**

[Tiếng Việt](README.vi.md)

Save several Google logins, see each account's plan and quota, and switch with one command. The `agy` CLI and Antigravity 2.x desktop app share one login in Windows Credential Manager (`gemini:antigravity`); this tool saves several logins and swaps that entry.

## Features

- Save accounts, add more through Google sign-in in the browser, rename them, and recover deleted profiles.
- Numbered, aligned `list` table and interactive `tui` dashboard with the plan, used percentages and relative reset times.
- Separate Claude/GPT 5-hour and weekly quotas and Gemini 5-hour and weekly quotas, from Google Cloud Code's `retrieveUserQuotaSummary`.
- Switch by alias, by number, or to the best account in the selected scoring pool; launch `agy` or use `auto` for automatic switching.

## Requirements

Windows only for now. macOS Keychain and Linux secret-service are not supported yet. Install `agy` to sign in and launch Antigravity.

## Install

```sh
npm i -g paper-agy-switch
```

The npm package downloads a prebuilt binary from [GitHub Releases](https://github.com/Cazorlas/Paper_agy_switcher/releases).

Or build from source:

```sh
cargo install --git https://github.com/Cazorlas/Paper_agy_switcher
```

Data lives in `~/.paper-agy-switch`; set `PAPER_AGY_SWITCH_HOME` to override this folder.

## Setup (first time)

1. Start `agy`, run `/login` inside it, and sign in with your first Google account.
2. Save that login:
   ```sh
   paper-agy-switch login main
   ```
3. Add another account:
   ```sh
   paper-agy-switch add work
   ```
   Sign in with Google in the browser. The login is saved straight to the profile: nothing to close and no `/logout` needed.
4. Check your accounts with `paper-agy-switch list`.

**Do not use `/logout` in agy for an account you saved:** it may revoke that saved login. To renew an expired login, run `/login` inside agy, then `paper-agy-switch login <alias>` again.

## Everyday use

```sh
paper-agy-switch list                  # numbered quota table
paper-agy-switch use main              # switch by alias
paper-agy-switch use 2                 # switch by number in list
paper-agy-switch use                   # best account in the scoring pool
paper-agy-switch launch work -- --continue # switch, then start agy with these arguments
paper-agy-switch tui                   # interactive dashboard and Settings
paper-agy-switch auto                  # automatic switching
paper-agy-switch doctor                # check setup and current login
```

Numbers follow alias order in `list`, matching `use <n>`. A literal numeric alias takes priority over a row number. `*` marks the active profile; reset cells such as `↻2h13m` and `↻5d4h` show time remaining, and `--` means absent data. Fetch errors appear in Status.

### Running sessions and credential overwrites

Measured limitation: an already running `agy` process keeps the account it started with. This includes an agy terminal, the Antigravity hub started by VS Code, and the desktop app. When that process refreshes its token (about hourly), it writes its account back into Credential Manager, undoing the switch. New agy processes use the switched account immediately.

`use` lists running agy processes. To stop them when switching:

```sh
paper-agy-switch use main --restart-agy
```

`auto` reports when the credential was overwritten.

### Scoring pool

Choose the pool used to select the best account in `~/.paper-agy-switch/config.toml`, or in TUI Settings:

```toml
[use]
pool = "claude" # default: Claude/GPT group; use "gemini" for Gemini
```

### Quota limitations

Measured behaviour: Antigravity can refuse a model with **"Individual quota reached"** even when the summary windows show room left. The dashboard shows the summary Google reports; it does not represent every limit.

## Manage accounts

| Task | Command |
|---|---|
| Save the current agy login | `paper-agy-switch login [alias]` |
| Add through browser sign-in | `paper-agy-switch add <alias>` |
| Refresh quota without the cache | `paper-agy-switch list --force` |
| Rename | `paper-agy-switch rename <old> <new>` |
| Delete, with recovery available | `paper-agy-switch delete <alias-or-number> [--yes]` |
| List deleted profiles | `paper-agy-switch restore` |
| Restore a profile | `paper-agy-switch restore <alias> [--as <new>]` |

## Update / uninstall

```sh
paper-agy-switch self-update --check
paper-agy-switch self-update
```

For a source installation, rerun the `cargo install --git` command above.

The **npm launcher** provides `uninstall`; it is not a command in the binary's `--help`:

```sh
paper-agy-switch uninstall             # asks whether to delete saved data in an interactive terminal
paper-agy-switch uninstall --keep-data # keep accounts and settings
paper-agy-switch uninstall --purge     # delete accounts and settings
```

## How it works

The tool saves logins in profiles under its data folder. Switching replaces the shared Windows Credential Manager entry `gemini:antigravity`, which new agy and Antigravity desktop processes read.

## Credits and license

Inspired by [Cockpit Tools](https://github.com/jlcodes99/cockpit-tools) for Antigravity protocol research (behaviour only; no code copied). Forked from [Paper.Claude-switcher](https://github.com/Cazorlas/Paper.Claude-switcher).

Apache-2.0 — see [LICENSE](LICENSE).
