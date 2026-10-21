#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
source "$SCRIPT_DIR/common.sh"

echo "starship_rprompt: starting (zsh=$(command -v zsh 2>/dev/null || echo MISSING), bash=$BASH_VERSION)" >&2

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/faz-starship-rprompt.XXXXXX")"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

mkdir -p "$tmp_dir/bin"
cat >"$tmp_dir/bin/starship" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
init)
  if [[ "${2:-}" != "zsh" ]]; then
    exit 1
  fi
  cat <<'OUT'
RPROMPT='$(echo fake-right-prompt)'
OUT
  ;;
prompt)
  if [[ "${2:-}" != "--right" ]]; then
    exit 1
  fi
  echo "fake-right-prompt"
  ;;
*)
  exit 1
  ;;
esac
EOF
chmod +x "$tmp_dir/bin/starship"

HOME="$tmp_dir/home"
ZDOTDIR="$HOME"
mkdir -p "$HOME"

# Provide stub vendor plugin dirs so setup_zsh.sh succeeds on CI where the
# real downloads are not present in the checkout.
vendor_dir="$tmp_dir/vendor"
create_stub_vendor_dir "$vendor_dir"
mkdir -p "$vendor_dir/zsh-syntax-highlighting"

echo "starship_rprompt: running setup_zsh.sh" >&2
setup_out=""
setup_status=0
setup_out="$(
  PATH="$tmp_dir/bin:$PATH" \
  HOME="$HOME" \
  ZDOTDIR="$ZDOTDIR" \
  FAZ_INIT_INTERNAL=1 \
  FAZ_SKIP_TOOL_BOOTSTRAP=1 \
  FAZ_SKIP_TERMINFO_BOOTSTRAP=1 \
  FAZ_VENDOR_DIR="$vendor_dir" \
  bash "$REPO_ROOT/assets/shell-integration/setup_zsh.sh" --update-only 2>&1
)" || setup_status=$?
if [[ "$setup_status" -ne 0 ]]; then
  echo "starship_rprompt: setup_zsh.sh failed (exit $setup_status):" >&2
  echo "$setup_out" >&2
  exit 1
fi

faz_zsh="$HOME/.config/faz/zsh/faz.zsh"
if [[ ! -f "$faz_zsh" ]]; then
  echo "starship_rprompt: faz.zsh not created at $faz_zsh" >&2
  exit 1
fi
echo "starship_rprompt: faz.zsh created ok, running zsh" >&2

output=""
if ! output="$(
  TERM=xterm-256color \
  TERM_PROGRAM=Faz \
  PATH="$tmp_dir/bin:$PATH" \
  HOME="$HOME" \
  ZDOTDIR="$ZDOTDIR" \
  zsh -f -c '
source "$HOME/.config/faz/zsh/faz.zsh"
RPROMPT='\''$(starship prompt --right)'\''
_faz_fix_starship_rprompt
print -r -- "__FAZ_RPROMPT__:$RPROMPT"
' 2>&1
)"; then
  echo "starship_rprompt: zsh exited non-zero:" >&2
  echo "$output" >&2
  exit 1
fi

if [[ -z "$output" ]]; then
  echo "starship_rprompt: zsh produced no output" >&2
  exit 1
fi

case "$output" in
  *__FAZ_RPROMPT__:* ) ;;
  * )
    echo "starship_rprompt: sentinel not found in output:" >&2
    echo "$output" >&2
    exit 1
    ;;
esac

case "$output" in
  *"closing brace expected"* | *"bad pattern"* )
    echo "starship_rprompt: zsh pattern error:" >&2
    echo "$output" >&2
    exit 1
    ;;
esac

seeded_output=""
if ! seeded_output="$(
  TERM=xterm-256color \
  TERM_PROGRAM=Faz \
  PATH="$tmp_dir/bin:$PATH" \
  HOME="$HOME" \
  ZDOTDIR="$ZDOTDIR" \
  zsh -f -c '
source "$HOME/.config/faz/zsh/faz.zsh"
RPROMPT='\''$(echo fake-right-prompt)'\''
_faz_starship_rprompt_cmd="$RPROMPT"
_faz_fix_starship_rprompt
print -r -- "__FAZ_SEEDED__:$RPROMPT"
' 2>&1
)"; then
  echo "starship_rprompt: seeded zsh exited non-zero:" >&2
  echo "$seeded_output" >&2
  exit 1
fi

case "$seeded_output" in
  *__FAZ_SEEDED__:fake-right-prompt* ) ;;
  * )
    echo "starship_rprompt: seeded sentinel missing or wrong output:" >&2
    echo "$seeded_output" >&2
    exit 1
    ;;
esac

case "$seeded_output" in
  *"closing brace expected"* | *"bad pattern"* )
    echo "starship_rprompt: seeded zsh pattern error:" >&2
    echo "$seeded_output" >&2
    exit 1
    ;;
esac

echo "starship_rprompt smoke test passed"
