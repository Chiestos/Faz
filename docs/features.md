# Features

## Terminal Interactions

Cmd+Click opens URLs and file paths, and also bare domains such as
`github.com` that have no scheme prefix. The matcher is tuned to leave code
identifiers alone: method calls like `df.info()` and namespaces like
`System.Net` never turn into links.

Option+Click moves the shell cursor within the current input line, including
across soft-wrapped continuation rows. It never crosses a hard newline, so
clicks into scrollback are ignored rather than mangling history.

---

## Window Snapshots

Faz saves multi-tab and multi-pane window layouts automatically when you close
or hide a window. Use **Shell > Restore Previous Window** or
`Cmd + Option + Shift + T` to reopen the last saved layout. Faz tolerates
missing or corrupted snapshot files and simply reports that no snapshot is
available.

---

## AppleScript

Faz ships a minimal AppleScript dictionary so it shows up in Script Editor and other automation tools. The exposed surface is intentionally small and read-only apart from `quit`.

```applescript
tell application "Faz"
  get name        -- "Faz"
  get version     -- e.g. "0.10.0"
  get frontmost   -- true / false
  quit            -- optional `saving ask|yes|no`
end tell
```

Open `/Applications/Faz.app` in Script Editor → File → Open Dictionary to browse the full dictionary. There is no `do script` verb — Faz does not expose shell execution to AppleScript.

---

## Lazygit Integration

Press `Cmd + Shift + G` to launch lazygit in the current pane. Faz auto-detects the lazygit binary from PATH or common Homebrew locations.

When a git repo has uncommitted changes and lazygit has not been used in that directory yet, Faz shows a one-time hint to remind you it is available.

Install lazygit with `brew install lazygit` or via `faz init`.

---

## Yazi File Manager

Press `Cmd + Shift + Y` to launch yazi in the current pane. The shell wrapper `y` also launches yazi and syncs the shell working directory on exit.

**Theme sync**: Faz automatically updates `~/.config/yazi/theme.toml` to match the active color scheme (Faz Dark or Faz Light). No manual yazi theme setup needed.

Install yazi with `brew install yazi` or via `faz init`.

---

## Remote Files

Press `Cmd + Shift + R` to mount the current SSH session's remote filesystem locally via `sshfs` and open it in yazi.

Faz auto-detects the SSH target from the active pane. The mount lives at `~/Library/Caches/dev.faz/sshfs/<host>`.

Requirements: `sshfs` installed (`brew install macfuse sshfs`) and passwordless SSH auth (key-based) for the remote host.

---

## Shell Suite

Faz ships a curated set of shell plugins that load automatically inside Faz sessions.

**Zsh plugins (built-in)**

- **z**: Smarter `cd` that learns your most-used directories. Use `z <dir>`, `z -l <dir>` to list matches, `z -t` for recent directories.
- **zsh-completions**: Extended completions for common CLI tools.
- **zsh-syntax-highlighting**: Real-time command coloring and error highlighting.
- **zsh-autosuggestions**: Fish-style history-based completions as you type.

**Fish support**

Run `faz init` to provision `~/.config/faz/fish/faz.fish` for fish users. `faz doctor` verifies both zsh and fish integration paths.

**Optional tools (installed via `faz init`)**

- **Starship**: Fast, customizable prompt with git and environment info.
- **Delta**: Syntax-highlighting pager for git diff and grep.
- **Lazygit**: Terminal git UI.
- **Yazi**: Terminal file manager.

**Smart Tab**

Faz's Smart Tab overrides the Tab key in zsh to provide smarter completion behavior. It supports three modes:

| Mode | Behavior | Environment Variable |
| :--- | :--- | :--- |
| Completion First | Tab shows the completion list; use `->` to accept autosuggestions | - |
| Suggestion First (default) | Tab accepts autosuggestions when available, falls back to completion | `FAZ_TAB_ACCEPT_SUGGEST_FIRST=1` |
| Off | Disables Smart Tab entirely, restoring native zsh Tab behavior | `FAZ_SMART_TAB_DISABLE=1` |

You can also set the mode via `faz config` (the **Smart Tab** option under Behavior) or in `faz.lua`:

```lua
config.smart_tab_mode = "suggestion_first"   -- default; Tab accepts autosuggestions first
config.smart_tab_mode = "completion_first"   -- Tab shows the completion list instead
config.smart_tab_mode = "off"                -- disable Smart Tab
```

If you prefer environment variables (for example, because you share your zshrc across terminals), add one of these before sourcing the Faz shell integration:

```zsh
export FAZ_TAB_ACCEPT_SUGGEST_FIRST=1  # suggestion-first mode
# or
export FAZ_SMART_TAB_DISABLE=1         # disable Smart Tab
```

```fish
set -gx FAZ_SMART_TAB_DISABLE 1
```

Environment variables set in your shell rc take precedence over `faz.lua` settings. Smart Tab is only active inside Faz sessions (`TERM_PROGRAM=Faz`).
