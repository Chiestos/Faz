#!/usr/bin/env bash
# Exercise report validation and the real measurement harness with a fake app.
set -euo pipefail
cd "$(dirname "$0")/.."
fixture=$(mktemp -d "${TMPDIR:-/tmp}/kaku-startup-test.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
check_status() {
  local expected="$1" actual=0
  BUDGET_FILE="$fixture/budget.toml" scripts/check_startup_budget.sh "$fixture/report.json" >"$fixture/check.log" 2>&1 || actual=$?
  [[ "$actual" == "$expected" ]] || { cat "$fixture/check.log"; echo "Expected $expected, got $actual"; exit 1; }
}
printf 'cold_start_budget_ms = 100\nwarm_start_budget_ms = 0\n' > "$fixture/budget.toml"
printf '{"schema_version":2,"measurement":"launch_to_prompt_and_first_paint","cold_start_ms":99.9,"warm_start_ms":1000}\n' > "$fixture/report.json"
check_status 0
jq '.cold_start_ms = 100.1' "$fixture/report.json" > "$fixture/next.json"
mv "$fixture/next.json" "$fixture/report.json"
check_status 1
for value in '"100"' null -1 0 '[]'; do
  jq --argjson value "$value" '.cold_start_ms = $value' "$fixture/report.json" > "$fixture/next.json"
  mv "$fixture/next.json" "$fixture/report.json"
  check_status 2
done
printf '{"cold_start_ms":1,"warm_start_ms":1}\n' > "$fixture/report.json"
check_status 2
printf 'not json\n' > "$fixture/report.json"
check_status 2
mkdir -p "$fixture/Fake.app/Contents/MacOS"
cat > "$fixture/Fake.app/Contents/MacOS/kaku-gui" <<'APP'
#!/usr/bin/env bash
if [[ "${1:-}" == --version ]]; then echo fixture; exit; fi
if [[ "${FAIL_STARTUP:-0}" == 1 ]]; then exit 1; fi
printf '[startup] 0 window.show() done\n'
printf 'ready\n' > "$KAKU_BENCH_PROMPT"
printf '[startup] 1 first paint_impl done\n'
while :; do sleep 1; done
APP
chmod +x "$fixture/Fake.app/Contents/MacOS/kaku-gui"
APP_PATH="$fixture/Fake.app" RUNS=2 WARMUP=0 scripts/measure_startup_kaku.sh > "$fixture/report.json" 2>"$fixture/measure.log"
jq -e '.schema_version == 2 and .runs == 2 and .version == "fixture" and .cold_ready.p95_ms > 0 and .warm_window.mean_ms > 0' "$fixture/report.json" >/dev/null
if FAIL_STARTUP=1 APP_PATH="$fixture/Fake.app" RUNS=2 WARMUP=0 scripts/measure_startup_kaku.sh > /dev/null 2>"$fixture/failed.log"; then
  echo "A failed startup must not produce a successful report" >&2
  exit 1
fi
echo "Startup harness and budget boundary checks passed"
