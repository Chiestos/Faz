# CLI Reference

Run `kaku` in your terminal to see all available commands.



## faz config

Open the Faz configuration TUI for common settings and Lua overrides. It
ensures `~/.config/faz/faz.lua` exists and is also accessible from the
settings panel with `Cmd + ,`.

```bash
faz config
```

## faz doctor

Run diagnostics and verify that Faz's shell integration, PATH entries, and optional tool installations are healthy. Use this first if something feels broken.

```bash
faz doctor
faz doctor --shell fish       # check fish even when $SHELL points to zsh
faz doctor --shell fish --fix # repair the selected integration
```

## faz update

Check for and install the latest Faz release.

```bash
faz update
```

## faz reset

Remove Faz-managed shell and tmux integration, Faz-managed git delta defaults,
selected Faz state, and managed theme blocks in `~/.config/faz/faz.lua`.
User-authored Lua outside managed blocks is preserved. Use with caution and run
`faz init` again if you want shell integration back.

```bash
faz reset
faz reset --shell fish # use fish for restart and restore guidance
```

## faz init

Set up Faz's shell integration for zsh or fish. When both shells are installed,
an interactive run asks which one to configure. Use `--shell` to make the choice
explicit in scripts or when `$SHELL` does not match your daily shell. Also
installs optional CLI tools (Starship, Delta, Lazygit, Yazi) via Homebrew.

```bash
faz init
faz init --shell fish
faz init --shell zsh --update-only
```

If the `kaku` command goes missing from your shell, restore it with:

```bash
/Applications/Faz.app/Contents/MacOS/faz init --update-only
exec zsh -l
```

## faz cli

Interact with the Faz multiplexer from scripts and external tools.

```bash
faz cli split-pane                          # split current pane
faz cli split-pane -- bash -c "echo hello"  # split and run a command
faz cli --help                              # list all subcommands
faz cli split-pane --help                   # help for a specific subcommand
```

Useful for integrating Faz with shell scripts or external coding agents that need to open panes or tabs programmatically.
