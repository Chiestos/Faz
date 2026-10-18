# Faz Agent Guide

Faz is a macOS-native terminal emulator derived from WezTerm and shaped around AI-assisted terminal workflows. This guide is the shared operating context for agents working in this repository.

## Repository Map

- `faz-cli/` - CLI entry points, command flows, and user-facing configuration commands.
- `faz-gui/` - GUI, rendering, window lifecycle, input, and mouse handling.
- `mux/` - tabs, panes, domains, and client/server state.
- `term/` - terminal emulation and screen buffer behavior.
- `termwiz/` - terminal UI primitives.
- `config/` - Lua config loading, schema behavior, proxy settings, and versioned defaults.
- `window/` - platform windowing layer.
- `lua-api-crates/` - Rust-to-Lua API bindings.
- `crates/` - shared utility crates.
- `assets/` - app resources, bundled config, shell integration, and vendor assets.
- `scripts/` - build, release, and validation helpers.
- `docs/` - user and developer documentation.
- `.agents/skills/` - the canonical home for project skills, and the only tracked copy: `.claude/` is gitignored here, so `.claude/skills/<name>` are relative symlinks pointing back into `.agents/`. Add a skill under `.agents/skills/` and symlink it, never the reverse, or it ships to nobody. Skills: `release`, `maintainer-sweep`, `product-docs`, `bugs` (proactive multi-entry UX / latent-defect sweep). Exception: `.claude/rules/macos.md` is force-added and tracked despite the ignore rule; anything else added under `.claude/` needs `git add -f` or it stays local-only.
- `.github/workflows/checks.yml` - fast correctness gates on pushes to `main` and on pull requests, with `paths-ignore` for `**.md` and assets, so a docs-only change gets no CI at all. Runs fmt, check, tests, and the log/prompt guards; clippy is scoped to the lint-opt-in crates, not the workspace.
- `.github/workflows/build-validation.yml` - release-shaped universal/bundle builds; runs on build-pipeline changes, daily, or on dispatch. `release.sh` preflight requires its latest run green.
- `.github/RELEASE_NOTES.md` - source for the GitHub Release title and body.

## Commands

```bash
make fmt
make fmt-check
make check
make test
make dev
make app
./scripts/build.sh
./scripts/check_config_release_readiness.sh
./scripts/check_release_config.sh
./scripts/check_release_notes.sh
```

`make fmt` and `make fmt-check` both shell out to `cargo +nightly fmt`, so they require the nightly toolchain. Use `make app` for GUI, rendering, and windowing verification because it builds the app bundle that users run.

## Working Rules

- Work on the current branch unless the maintainer asks for a branch or worktree.
- Keep changes inside one crate or subsystem when the problem allows it.
- Prefer targeted `rg` searches over repository-wide scans.
- Inspect public APIs and cross-crate boundaries before changing shared behavior.
- Draft issue and PR replies unless the maintainer has already approved the exact public action.
- Do not modify files outside this repository without showing the intended change and getting explicit confirmation.
- Do not add instructions for the removed `website/` tree unless that directory exists in the current worktree.
- The marketing and docs site lives on the `vercel` branch (linked worktree at `~/www/faz-site`), not on `main`. It follows the design guide at `~/www/faz-site/DESIGN.md`; verify changes with screenshots at 375px / 1280px and deploy by pushing the `vercel` branch (Vercel serves kaku.fun).
- Keep private credentials, local keychain paths, and machine-specific release notes out of public repository docs.
- **Do not propose UI i18n / multi-language menus / a `config.language` setting.** The `rust-i18n` based Chinese UI localization (PR #362, commit `f6cfb4b`) was reverted on 2026-05-18; `language` remains in the config schema as a deprecated field for backward compat only. UI strings (menus, confirm dialogs, config TUI copy) stay as English literal strings. New UI surfaces should not introduce translation keys, locale-aware formatting, or "what if a user wants Chinese" abstractions. If a user requests a non-English UI, route to the shell integration or documentation; there is no in-app translation surface.

## Maintainer Follow-up

- For current issue and PR sweeps, read live GitHub state first with `gh issue list` and `gh pr list`; refresh once more before final conclusions or public actions.
- Before commenting on or closing an item, confirm its title, state, and author with `gh issue view` or `gh pr view`.
- Do not close issues or PRs on local green alone. For fixes pushed to `main`, wait for the new GitHub Actions run on `main` to pass before posting fixed/closed replies.
- The rolling `nightly` release is not rebuilt by push. Before sending users to Nightly, run or verify `./scripts/nightly.sh` and confirm `gh release view nightly --json tagName,targetCommitish,publishedAt,assets,url` points at the fix commit and includes `Faz-nightly.dmg`.
- Default issue-closure pipeline once a fix is verified: commit the fix, refresh the `nightly` release assets per the rule above, reply in the reporter's language with the Nightly download link, then propose closure and wait for maintainer confirmation. Do not promise a specific packaged-release date in replies.
- Before pushing `main`, run `git fetch origin main` and verify `origin/main` has not moved unexpectedly. If it moved, stop and review `origin/main..HEAD` before pushing.
- If an accepted PR's equivalent fix lands on `main` outside the contributor branch, state the landed commit and co-author status in the PR before closing it.

## Investigation Order

When scope is incomplete, inspect in this order:

1. User-provided repro, failing command, or failing test.
2. Entry point for the behavior, usually `faz-cli/src/main.rs`, `faz-cli/src/cli/`, or `faz-gui/src/main.rs`.
3. Owning subsystem document and target crate.
4. Immediate cross-crate boundary used by the call path.
5. Narrow tests, fixtures, snapshots, or scripts that reproduce the behavior.



For `Ctrl+letter` not working in a raw-mode TUI (the most common shape: `Ctrl+C` / `Ctrl+R` works in plain shell but not inside a TUI overlay), inspect in this order:

1. AppKit menu `keyEquivalent` intercepting `keyDown` before the terminal sees it. Enable `config.debug_key_events = true`, restart the app, then `grep 'key_event.*CTRL' ~/.local/share/faz/faz-gui-log-<pid>.txt`. If the log shows only `key_is_down: false` and no matching `key_is_down: true`, the AppKit menu absorbed the event; do not chase termwiz or PTY.
2. Cooked-mode tests (`cat -v` showing `^C`) do **not** rule out menu interception. Reproduce inside a raw-mode TUI before forming a hypothesis.
3. Only after step 1 rules out menu interception, inspect termwiz encoding (`termwiz/src/input.rs`), then PTY / termios state.

For TUI display corruption after interactive CLIs re-render prompts or selection lists, first capture a minimal ANSI transcript. Add a terminal-core regression around cursor-up (`CSI n A`), full-line erase (`CSI 2K`), cursor-down (`CSI 1B`), wrapped rows, and styled prompt symbols. If the core transcript passes but the built app differs from Terminal.app, inspect GUI width, cell metrics, resize, and wrapping inputs rather than changing terminal semantics blindly.

## Subsystem Guides

| Subsystem | Guide | Scope |
|---|---|---|
| GUI | `faz-gui/AGENTS.md` | Rendering, window lifecycle, input, mouse |
| Mux | `mux/AGENTS.md` | Tabs, panes, domains, client/server |
| Terminal | `term/AGENTS.md` | VT emulation, screen buffer |
| Config | `config/AGENTS.md` | Lua loading, schema, config reload |
| Termwiz | `termwiz/AGENTS.md` | TUI primitives and widgets |
| Lua API | `lua-api-crates/AGENTS.md` | Rust-to-Lua bindings |
| Crates | `crates/AGENTS.md` | Shared utility crates |
| macOS platform | `.claude/rules/macos.md` | AppKit menu / keyEquivalent traps, menubar init timing |

## Verification

| Change type | Command |
|---|---|
| Rust compile check | `make check` |
| Rust logic change | `make test` |
| Formatting | `make fmt-check` |
| GUI or rendering change | `make app` |
| Config release change | `./scripts/check_config_release_readiness.sh` and `./scripts/check_release_config.sh` |
| Release note change | `./scripts/check_release_notes.sh` |
| Release-adjacent change | `make fmt && make check && make test`, then `make app` |
| `crates/faz-relay` change | `cargo check --locked --manifest-path crates/faz-relay/Cargo.toml` (it is in `workspace.exclude`, so `make check` and `make test` never see it; CI's `relay-check` job is the only gate) |

For GUI or rendering issues, read `faz-gui/AGENTS.md` first and verify with `make app`, not only `make dev`.

## Current Risk Areas

- There is no in-app update surface: no menu item, no background check, no `faz update`, and no Homebrew tap. Updates come from downloading a new DMG. Do not reintroduce an update path without an explicit product decision; details in `faz-gui/AGENTS.md`.
- Shell integration is an active product surface. Preserve proxy config and the shell user-var contract that user configs subscribe to.
- `config_version` bumps every release; the source of truth is `assets/shell-integration/config_version.txt` and the gate is `scripts/check_release_config.sh`. Config schema changes must update bundled defaults, docs, and release checks together. **There is no historical config migration**: a removed key is kept as a tolerated `deprecated` field or ignored, never rewritten in the user's file, so do not add version-gated branches. Per-version history lives in `docs/config-versions.md`. Do not hardcode the current version number in agent guides; it goes stale between releases.
- GUI regressions can come from overlay resize, pane split/removal, macOS worker thread lifetime, WebGPU surface reconfigure, tab bar spacing, and alternate-screen wheel scroll behavior.
- Startup performance depends on caching shell user vars, Lua bytecode, early appearance queries, GLSL version, and built-in fonts. Do not invalidate those caches without measurement.
- Notification actions that call back into Faz should resolve bundled executables relative to the running app, not an assumed system path.
- Known high-regression zones: theme/config TUI initialization (`faz-cli/src/config_tui/`), macOS window geometry (`window/src/os/macos/window.rs`, `faz-gui/src/termwindow/resize.rs`), and menubar initialization timing (`window/src/os/macos/menu.rs`). Changes touching these must ship with a regression test or assertion.
- `assets/shell-integration` scripts run in the user's shell, not just at build time. Bash heredocs that generate zsh (e.g. `setup_zsh.sh` writing `faz.zsh`) expand backticks and `$(...)` at generation time, so escape any that must reach the output literally (#450), and never put `local` outside a function (#432/#441). CI gates this: shellcheck (`--severity=error`, catches SC2168) over the bash scripts plus a `zsh -n` parse check of the generated `faz.zsh` in the setup smoke.

## Release Notes

Tag format is `V0.x.x`. `scripts/release.sh` is the source of truth for tagged releases. The GitHub Release title comes from the first heading in `.github/RELEASE_NOTES.md`.

Before drafting release notes, read the previous formal release (`gh release view <latest-tag>`) and treat it as the format template: title `V{version} {Codename}` with a one-word codename, a centered logo header with the product tagline, `### Changelog` as numbered `**Label**: one sentence` items, a `### 更新日志` section whose Chinese items map one-to-one by number, and a `> https://github.com/tw93/Faz` footer. After publishing, add all six positive reactions (`+1`, `laugh`, `heart`, `hooray`, `rocket`, `eyes`) to the release via `gh api` and read them back to confirm; never add `-1` or `confused`.

## Pre-release Runtime Smoke

CI gates fmt/check/clippy/tests but cannot see visual layout, native AppKit, render timing, or shell-in-user-env behavior, which is where most post-release reports come from. The checklist lives in one place: `.agents/skills/release/SKILL.md` «Pre-release smoke checklist», covering macOS window, tab bar, shell setup, and render timing, with the issue numbers each item guards and a note on which slices now have automated coverage. Run it by hand in the built `dist/Faz.app` before tagging, and add the reproduction there when a release fixes a bug outside the list.

Releases that touch windowing, titlebar coloring, tab bar layout, or transparency also need the config matrix in `docs/release-checklist.md`, which enumerates the tab position / tab style / opacity / window state combinations to check by hand and names the `update_titlebar_background()` regression guard.

## Documentation Maintenance

- Single-crate behavior belongs in that crate's `AGENTS.md`.
- Cross-crate behavior should update every affected subsystem guide.
- Build, CI, release, and maintainer workflow changes belong in this root file.
- Shared agent instructions belong in tracked docs. Personal overrides belong in ignored local files.
- One-off review reports, scorecards, and diagnostic snapshots are evidence, not durable project docs. Extract stable rules or verification gates into `AGENTS.md`, `CLAUDE.md`, subsystem guides, scripts, or tests, then remove the transient report.
- Do not hide user-visible behavior changes inside maintainability or cleanup patches. New UI, config fields, defaults, or workflow permissions should be split into their own change unless the maintainer explicitly approved that scope.
