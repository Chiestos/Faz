# Mux Agent Guide

The `mux` crate owns tab, pane, domain, and mux state management.

## Scope

`mux` is the central abstraction for:
- tabs and panes
- windows and workspaces
- local and remote domains
- client/server mux coordination

## Where to Look

- `mux/src/tab.rs`: tab tree and split behavior
- `mux/src/pane.rs`: pane state and interfaces
- `mux/src/window.rs`: window-level mux state
- `mux/src/domain.rs`: domain abstractions
- `mux/src/client.rs` and `crates/wezterm-mux-server-impl/src/` (`sessionhandler.rs`, `dispatch.rs`, `local.rs`): protocol flow

## Practical Rules

- Keep alert propagation reliable, including `Alert::SetUserVar`, because GUI reload and user-var hooks depend on this path.
- Preserve pane and overlay invalidation signals consumed by GUI AI/chat overlays when panes split, move, close, or resize.

## Known Pitfalls

- **Alert propagation**: `Alert::SetUserVar` must reach the GUI side reliably; broken propagation silently breaks config reload and user-var event hooks.
- **Pane ID stability**: Pane IDs must remain stable across mux round-trips; don't reassign or reuse IDs in ways that confuse client/server sync.
- **Domain abstraction leakage**: GUI-specific logic (rendering, window handles) must not appear in `mux`; only mux-level state and events.
- **Split tree correctness**: Tab split tree mutations in `tab.rs` are tricky; always verify with a pane layout sanity check after modifications.
- **Overlay resize regressions**: GUI overlays derive geometry from pane state. Mux changes that skip resize or removal notifications can leave AI/chat overlays stale.

## tmux Control Mode

`tmux -CC` is a live surface. The pieces that are easy to break:

- **One window per session.** `TmuxDomain` is named `tmux-<pane_id>` so several sessions never collide in the mux name index. The window is created with `new_empty_window` and published only after the first tab lands: `MuxWindowBuilder` fires `WindowCreated` from `Drop`, so holding the builder forever keeps the window invisible.
- **Input path.** `TmuxPty::write` must report the full buffer length. Returning `Ok(0)` makes `write_all` stop with `WriteZero`, which looks like "typing stalls after one character".
- **Detach writes to the pty, not the terminal.** For a tmux pane the terminal writer is a `TmuxPtyWriter` that converts everything into `send-keys` for a remote pane. `LocalPane::detach_tmux` writes `detach\n` to `self.writer` so the command reaches the tmux client.
- **Closing the window detaches.** On macOS `close_requested` hides windows with `order_out()`, which keeps the client attached; a window hosting tmux panes takes the real close path instead. `remove_window_internal` must drop the window lock before calling `detach`, which writes to a pty.
- **Titles belong to tmux.** Tmux tabs skip the user's `format-tab-title` on both the batch and hover paths, and inner-shell `OSC 2` must not overwrite the remote window name.

## Cross-References

- [`faz-gui/AGENTS.md`](../faz-gui/AGENTS.md) - GUI rendering for mux state.
- [`lua-api-crates/AGENTS.md`](../lua-api-crates/AGENTS.md) - Lua APIs for mux control.
- [`term/AGENTS.md`](../term/AGENTS.md) - Terminal instances owned by panes.
