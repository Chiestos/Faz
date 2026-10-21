#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

fail() {
  echo "kiro_autosuggest_compat: $*" >&2
  exit 1
}

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/faz-kiro-autosuggest.XXXXXX")"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

HOME="$tmp_dir/home"
ZDOTDIR="$HOME"
mkdir -p "$HOME"

bin_dir="$tmp_dir/bin"
mkdir -p "$bin_dir"
cat >"$bin_dir/kiro-cli" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$bin_dir/kiro-cli"

vendor_dir="$tmp_dir/vendor"
mkdir -p "$vendor_dir/fast-syntax-highlighting" \
         "$vendor_dir/zsh-autosuggestions" \
         "$vendor_dir/zsh-completions/src" \
         "$vendor_dir/zsh-z"

setup_path="$bin_dir:/usr/bin:/bin:/usr/sbin:/sbin"
setup_out=""
setup_status=0
setup_out="$(
  PATH="$setup_path" \
  HOME="$HOME" \
  ZDOTDIR="$ZDOTDIR" \
  FAZ_INIT_INTERNAL=1 \
  FAZ_SKIP_TOOL_BOOTSTRAP=1 \
  FAZ_SKIP_TERMINFO_BOOTSTRAP=1 \
  FAZ_VENDOR_DIR="$vendor_dir" \
  bash "$REPO_ROOT/assets/shell-integration/setup_zsh.sh" --update-only 2>&1
)" || setup_status=$?
if [[ "$setup_status" -ne 0 ]]; then
  echo "$setup_out" >&2
  fail "setup_zsh.sh failed with exit $setup_status"
fi

faz_zsh="$HOME/.config/faz/zsh/faz.zsh"
[[ -f "$faz_zsh" ]] || fail "managed init file not created at $faz_zsh"

grep -Fq 'typeset -g _faz_autosuggest_cli_provider="kiro-cli"' "$faz_zsh" \
  || fail "managed init did not record kiro-cli compatibility mode"

if grep -Fq 'source "$FAZ_ZSH_DIR/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh"' "$faz_zsh"; then
  fail "managed init still sources bundled zsh-autosuggestions in kiro compatibility mode"
fi

grep -Fq 'if [[ "${_faz_external_autosuggest_provider:-0}" != "1" ]] && (( ${+widgets[autosuggest-accept]} )) && [[ -n "${POSTDISPLAY:-}" ]]; then' "$faz_zsh" \
  || fail "managed Tab widget is missing the external autosuggest compatibility guard"

runtime_out=""
if ! runtime_out="$(
  PATH="$setup_path" \
  TERM=xterm-256color \
  HOME="$HOME" \
  ZDOTDIR="$ZDOTDIR" \
  zsh -f -c '
source "$HOME/.config/faz/zsh/faz.zsh"
print -r -- "__FAZ_AUTOSUGGEST_PROVIDER__:${_faz_autosuggest_cli_provider:-}"
print -r -- "__FAZ_EXTERNAL_AUTOSUGGEST__:${_faz_external_autosuggest_provider:-0}"
' 2>&1
)"; then
  echo "$runtime_out" >&2
  fail "sourcing generated faz.zsh failed"
fi

case "$runtime_out" in
  *__FAZ_AUTOSUGGEST_PROVIDER__:kiro-cli* ) ;;
  * ) echo "$runtime_out" >&2; fail "runtime provider marker was not preserved" ;;
esac

case "$runtime_out" in
  *__FAZ_EXTERNAL_AUTOSUGGEST__:1* ) ;;
  * ) echo "$runtime_out" >&2; fail "runtime compatibility flag was not enabled" ;;
esac

echo "kiro_autosuggest_compat smoke test passed"
