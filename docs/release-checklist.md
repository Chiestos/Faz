# Release Checklist

## macOS Tab Bar Matrix

Before shipping a release that touches windowing, titlebar coloring, tab bar
layout, or transparency, build the app with `make app` and verify these macOS
config combinations manually:

| Tab position | Tab style | Opacity | Window state | Expected result |
| --- | --- | --- | --- | --- |
| Top | Fancy | Opaque | Windowed | Tab text/icons stay visible below integrated traffic lights. |
| Top | Fancy | Transparent | Windowed | Tab text/icons stay visible; transparent titlebar has no gap. |
| Top | Retro | Opaque | Windowed | Tab text/icons stay visible below integrated traffic lights. |
| Top | Retro | Transparent | Windowed | Tab text/icons stay visible; transparent titlebar has no gap. |
| Bottom | Fancy | Opaque | Windowed | Bottom tab bar is visible and top content clears traffic lights. |
| Bottom | Fancy | Transparent | Windowed | Bottom tab bar is visible; top titlebar area has no gap. |
| Bottom | Retro | Opaque | Windowed | Bottom tab bar is visible and top content clears traffic lights. |
| Bottom | Retro | Transparent | Windowed | Bottom tab bar is visible; top titlebar area has no gap. |
| Top | Fancy | Opaque | Fullscreen | Native titlebar does not cover the rendered tab bar. |
| Bottom | Fancy | Opaque | Fullscreen | Bottom tab bar remains visible after entering and leaving fullscreen. |

The key regression guard is `update_titlebar_background()` in
`window/src/os/macos/window.rs`: native titlebar coloring must remain opt-in for
opaque windows, otherwise `NSTitlebarContainerView` can cover the Metal-rendered
top tab bar.

## Repeatable Performance Checks

Use the same release build, display scale, window size, power mode, and machine
for before/after samples. Record the source revision and build profile. Debug
builds prove the harness works but are not release latency baselines.

| Scenario | Fixed workload | Evidence |
| --- | --- | --- |
| Startup | `RUNS=10 WARMUP=2 scripts/measure_startup_kaku.sh > startup.json` | Window and shell-ready mean, median and P95; cold resets only the fixture app cache. |
| Sustained output | One 80x24 pane, 100,000 lines of 79 ASCII characters plus newline, repeated three times | PTY byte rate, mux action latency, paint P95, peak RSS. |
| Multiple panes | Four panes at the same total window size, each printing the same output workload | Per-process CPU/RSS and paint P95; confirm input remains responsive in a fifth idle pane. |
| AI streaming | A local OpenAI-compatible fixture emitting 20 fixed-size deltas/second for 30 seconds, repeated three times | Paint P95 and CPU/RSS during streaming; cancel halfway once and confirm work stops. Network/provider latency is excluded. |

Enable `periodic_stat_logging = 5` in an isolated test config to collect existing
`gui.paint.impl`, `read_from_pane_pty.bytes.rate`, and
`send_actions_to_mux.perform_actions.latency` metrics. Use a fresh process per
scenario so percentile history does not mix workloads. The output workload is:

```sh
python3 -c 'import sys; sys.stdout.write(("x" * 79 + "\n") * 100000); sys.stdout.flush()'
```

Run `bash scripts/test_startup.sh` after changing measurement or budget logic.
The schema 2 startup budget checks launch-to-shell-ready means; zeros disable
individual budgets until a release-build baseline exists. Preparation and
process cleanup run outside the timed interval. The cross-terminal comparison
script refuses to run while a target terminal is already open, and only
terminates processes it started.
