#!/usr/bin/env bash
# Smoke test: tools already managed by mise must not be queued for brew install.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SCRIPT="$SCRIPT_DIR/../install_cli_tools.sh"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/faz-mise-smoke.XXXXXX")"
cleanup() {
	rm -rf "$tmp_dir"
}
trap cleanup EXIT

# Build a fake HOME with a mise shim for starship.
fake_home="$tmp_dir/home"
mise_shims="$tmp_dir/mise/shims"
mkdir -p "$fake_home" "$mise_shims"

cat <<'EOF' >"$mise_shims/starship"
#!/bin/sh
echo "stub-starship"
EOF
chmod +x "$mise_shims/starship"

# Run the install script with:
#   - PATH stripped down so `command -v starship` and `brew` both fail.
#   - MISE_DATA_DIR pointing at our fake shim directory.
#   - Non-interactive (no TTY), so the script cannot prompt.
output="$(
	HOME="$fake_home" \
	MISE_DATA_DIR="$tmp_dir/mise" \
	PATH="/usr/bin:/bin" \
	bash "$INSTALL_SCRIPT" 2>&1 || true
)"

# The script must not print "Installing:" (which precedes the brew install call).
if echo "$output" | grep -q "^Installing:"; then
	echo "FAIL: script attempted to install tools already managed by mise"
	echo "--- script output ---"
	echo "$output"
	exit 1
fi

echo "install_cli_tools_mise smoke test passed"
