#!/usr/bin/env bash
# scripts/check_startup_budget.sh
#
# Read a JSON report from scripts/measure_startup_kaku.sh and check the
# numbers against the budget defined in scripts/startup_budget.toml. Exit 1 if
# any measurement exceeds budget; exit 0 if within budget; exit 2 if input
# is malformed.
#
# Usage:
#   scripts/check_startup_budget.sh path/to/startup.json
#   BUDGET_FILE=custom_budget.toml scripts/check_startup_budget.sh report.json
#
# The budget file is a tiny TOML with two integer keys:
#   cold_start_budget_ms = <number>
#   warm_start_budget_ms = <number>
#
# Budgets are derived from the hyperfine *mean* (see measure_startup_kaku.sh),
# not a P95. This is a local-only helper; the CI gate is intentionally not
# wired up yet (no committed baseline).

set -euo pipefail

json_file="${1:-}"
budget_file="${BUDGET_FILE:-scripts/startup_budget.toml}"

if [[ -z "$json_file" || ! -f "$json_file" ]]; then
  echo "usage: $0 <json_report> (BUDGET_FILE=<toml>)" >&2
  exit 2
fi

if [[ ! -f "$budget_file" ]]; then
  echo "ERROR: budget file '$budget_file' not found" >&2
  exit 2
fi

if ! jq -e '.schema_version == 2 and .measurement == "launch_to_prompt_and_first_paint" and ([.cold_start_ms, .warm_start_ms] | all(type == "number" and . > 0 and . < 1e12))' "$json_file" >/dev/null 2>&1; then
  echo "ERROR: expected a schema 2 report with positive numeric startup times" >&2
  exit 2
fi
cold=$(jq -r '.cold_start_ms' "$json_file")
warm=$(jq -r '.warm_start_ms' "$json_file")

cold_max=$(awk -F= '/^cold_start_budget_ms[[:space:]]*=/ {gsub(/[[:space:]]/,"",$2); print $2}' "$budget_file")
warm_max=$(awk -F= '/^warm_start_budget_ms[[:space:]]*=/ {gsub(/[[:space:]]/,"",$2); print $2}' "$budget_file")

if [[ ! "$cold_max" =~ ^[0-9]+$ || ! "$warm_max" =~ ^[0-9]+$ ]]; then
  echo "ERROR: budgets must be nonnegative integers, with each key defined once" >&2
  exit 2
fi

if [[ "$cold_max" =~ ^0+$ && "$warm_max" =~ ^0+$ ]]; then
  echo "WARN: budget is 0 (placeholder). Run scripts/measure_startup_kaku.sh"
  echo "      locally over ~10 runs, take the reported mean, multiply by 1.5,"
  echo "      and write the result into $budget_file before turning this gate hard."
  exit 0
fi

status=0
if awk -v value="$cold" -v budget="$cold_max" 'BEGIN { exit !(budget > 0 && value > budget) }'; then
  echo "FAIL: cold_start ${cold}ms exceeds budget ${cold_max}ms"
  status=1
fi
if awk -v value="$warm" -v budget="$warm_max" 'BEGIN { exit !(budget > 0 && value > budget) }'; then
  echo "FAIL: warm_start ${warm}ms exceeds budget ${warm_max}ms"
  status=1
fi

if [[ $status -eq 0 ]]; then
  echo "OK: cold=${cold}ms (budget ${cold_max}ms), warm=${warm}ms (budget ${warm_max}ms)"
fi
exit $status
