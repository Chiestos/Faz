#!/bin/bash
# Faz Zsh Setup Script
# This script configures a "batteries-included" Zsh environment using Faz's bundled resources.
# It is designed to be safe: it backs up existing configurations and can be re-run.

set -euo pipefail

UPDATE_ONLY=false
for arg in "$@"; do
	case "$arg" in
	--update-only)
		UPDATE_ONLY=true
		;;
	esac
done

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Thin entrypoint: delegate to `faz init` whenever possible.
# The rust command owns wrapper installation and orchestration.
if [[ "${FAZ_INIT_INTERNAL:-0}" != "1" ]]; then
	if [[ -n "${FAZ_BIN:-}" && -x "${FAZ_BIN}" ]]; then
		exec "${FAZ_BIN}" init --shell zsh "$@"
	fi

	for candidate in \
		"$SCRIPT_DIR/../MacOS/faz" \
		"/Applications/Faz.app/Contents/MacOS/faz" \
		"$HOME/Applications/Faz.app/Contents/MacOS/faz"; do
		if [[ -x "$candidate" ]]; then
			exec "$candidate" init --shell zsh "$@"
		fi
	done
fi

# Resolve resources by script location first so setup works regardless of app install path.
# - App bundle:   setup_zsh.sh in Resources/, vendor in Resources/vendor
# - Dev checkout: setup_zsh.sh in assets/shell-integration/, vendor in assets/vendor
if [[ -d "$SCRIPT_DIR/vendor" ]]; then
	RESOURCES_DIR="$SCRIPT_DIR"
elif [[ -d "$SCRIPT_DIR/../vendor" ]]; then
	RESOURCES_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
elif [[ -d "/Applications/Faz.app/Contents/Resources/vendor" ]]; then
	RESOURCES_DIR="/Applications/Faz.app/Contents/Resources"
elif [[ -d "$HOME/Applications/Faz.app/Contents/Resources/vendor" ]]; then
	RESOURCES_DIR="$HOME/Applications/Faz.app/Contents/Resources"
else
	echo -e "${YELLOW}Error: Could not locate Faz resources (vendor directory missing).${NC}"
	exit 1
fi

VENDOR_DIR="$RESOURCES_DIR/vendor"
# Allow test override so CI can provide stub plugin dirs without real downloads.
if [[ -n "${FAZ_VENDOR_DIR:-}" && -d "${FAZ_VENDOR_DIR}" ]]; then
	VENDOR_DIR="${FAZ_VENDOR_DIR}"
fi
TOOL_INSTALL_SCRIPT="$SCRIPT_DIR/install_cli_tools.sh"
if [[ ! -f "$TOOL_INSTALL_SCRIPT" ]]; then
	TOOL_INSTALL_SCRIPT="$RESOURCES_DIR/install_cli_tools.sh"
fi
USER_CONFIG_DIR="$HOME/.config/faz/zsh"
FAZ_INIT_FILE="$USER_CONFIG_DIR/faz.zsh"
FAZ_TMUX_DIR="$HOME/.config/faz/tmux"
FAZ_TMUX_FILE="$FAZ_TMUX_DIR/faz.tmux.conf"
STARSHIP_CONFIG="$HOME/.config/starship.toml"
YAZI_CONFIG_DIR="$HOME/.config/yazi"
YAZI_CONFIG_FILE="$YAZI_CONFIG_DIR/yazi.toml"
YAZI_KEYMAP_FILE="$YAZI_CONFIG_DIR/keymap.toml"
YAZI_THEME_FILE="$YAZI_CONFIG_DIR/theme.toml"
YAZI_FLAVORS_DIR="$YAZI_CONFIG_DIR/flavors"
YAZI_WRAPPER_FILE="$USER_CONFIG_DIR/bin/yazi"
FAZ_YAZI_THEME_MARKER_START="# ===== Faz Yazi Flavor (managed) ====="
FAZ_YAZI_THEME_MARKER_END="# ===== End Faz Yazi Flavor (managed) ====="
ZSHRC="${ZDOTDIR:-$HOME}/.zshrc"
TMUXRC="$HOME/.tmux.conf"
BACKUP_SUFFIX=".faz-backup-$(date +%s)"
ZSHRC_BACKED_UP=0
TMUXRC_BACKED_UP=0

if [[ -d "$SCRIPT_DIR/yazi-flavors" ]]; then
	FAZ_YAZI_FLAVOR_SOURCE_DIR="$SCRIPT_DIR/yazi-flavors"
else
	FAZ_YAZI_FLAVOR_SOURCE_DIR="$RESOURCES_DIR/yazi-flavors"
fi

backup_zshrc_once() {
	if [[ -f "$ZSHRC" ]] && [[ "$ZSHRC_BACKED_UP" -eq 0 ]]; then
		cp "$ZSHRC" "$ZSHRC$BACKUP_SUFFIX"
		ZSHRC_BACKED_UP=1
	fi
}

backup_tmuxrc_once() {
	if [[ -f "$TMUXRC" ]] && [[ "$TMUXRC_BACKED_UP" -eq 0 ]]; then
		cp "$TMUXRC" "$TMUXRC$BACKUP_SUFFIX"
		TMUXRC_BACKED_UP=1
	fi
}

default_kaku_config_path() {
	if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
		printf '%s\n' "${XDG_CONFIG_HOME}/faz/faz.lua"
	else
		printf '%s\n' "${HOME}/.config/faz/faz.lua"
	fi
}

active_kaku_config_path() {
	if [[ -n "${FAZ_CONFIG_FILE:-}" ]]; then
		printf '%s\n' "${FAZ_CONFIG_FILE}"
	else
		default_kaku_config_path
	fi
}

system_kaku_flavor() {
	local flavor="faz-dark"
	if command -v defaults >/dev/null 2>&1; then
		local appearance
		appearance="$(defaults read -g AppleInterfaceStyle 2>/dev/null || true)"
		if [[ "$appearance" != "Dark" ]]; then
			flavor="faz-light"
		fi
	fi
	printf '%s\n' "$flavor"
}

resolve_kaku_flavor_from_config() {
	local config_file="$1"
	local system_flavor
	system_flavor="$(system_kaku_flavor)"

	if [[ -f "$config_file" ]]; then
		local scheme_line
		scheme_line="$(
			awk '
				/^[[:space:]]*--/ { next }
				/^[[:space:]]*config\.color_scheme[[:space:]]*=/ { print; exit }
			' "$config_file"
		)"
		if [[ -n "$scheme_line" ]]; then
			if [[ "$scheme_line" == *"Faz Light"* ]]; then
				printf '%s\n' "faz-light"
				return
			fi
			if [[ "$scheme_line" == *"Faz Dark"* || "$scheme_line" == *"Faz Theme"* ]]; then
				printf '%s\n' "faz-dark"
				return
			fi
			if [[ "$scheme_line" == *"'Auto'"* || "$scheme_line" == *'"Auto"'* ]]; then
				printf '%s\n' "$system_flavor"
				return
			fi
			if [[ "$scheme_line" == *get_appearance* ]]; then
				printf '%s\n' "$system_flavor"
				return
			fi
			printf '%s\n' "faz-dark"
			return
		fi
	fi

	printf '%s\n' "$system_flavor"
}

current_kaku_yazi_flavor() {
	resolve_kaku_flavor_from_config "$(active_kaku_config_path)"
}

_faz_yazi_theme_block() {
	local flavor="${1:-$(current_kaku_yazi_flavor)}"
	cat <<EOF
$FAZ_YAZI_THEME_MARKER_START
[flavor]
dark = "$flavor"
light = "$flavor"
$FAZ_YAZI_THEME_MARKER_END
EOF
}

is_legacy_kaku_yazi_theme_file() {
	if [[ ! -f "$YAZI_THEME_FILE" ]]; then
		return 1
	fi

	local normalized expected
	if grep -Fq '# Faz-aligned theme for Yazi 26.x' "$YAZI_THEME_FILE"; then
		return 0
	fi
	normalized="$(sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*$/d' "$YAZI_THEME_FILE")"
	expected=$'[mgr]\nborder_symbol = "│"\nborder_style = { fg = "#555555" }\n[indicator]\npadding = { open = "", close = "" }'
	[[ "$normalized" == "$expected" ]]
}

# yazi >= 26.5.6 rejects a `$schema` key in its config files; the supported
# form is a `#:schema <url>` comment. Older Faz versions wrote the key, which
# makes yazi fail to start, so rewrite those lines in existing user configs.
migrate_yazi_schema_headers() {
	local file tmp_file
	for file in "$YAZI_THEME_FILE" "$YAZI_KEYMAP_FILE" "$YAZI_CONFIG_FILE"; do
		[[ -f "$file" && -w "$file" ]] || continue
		grep -Eq '^[[:space:]]*"?\$schema"?[[:space:]]*=' "$file" || continue
		tmp_file="$(mktemp "${TMPDIR:-/tmp}/faz-yazi-schema.XXXXXX")" || continue
		if sed -E \
			-e 's|^[[:space:]]*"?\$schema"?[[:space:]]*=[[:space:]]*"([^"]+)".*$|#:schema \1|' \
			-e '/^[[:space:]]*"?\$schema"?[[:space:]]*=/d' \
			"$file" >"$tmp_file"; then
			mv "$tmp_file" "$file"
			echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Migrated yazi \$schema key to #:schema comment ${NC}($(basename "$file"))${NC}"
		else
			rm -f "$tmp_file"
		fi
	done
}

sync_kaku_yazi_flavors() {
	if [[ ! -d "$FAZ_YAZI_FLAVOR_SOURCE_DIR" ]]; then
		echo -e "${YELLOW}Warning: bundled Yazi flavors are missing at $FAZ_YAZI_FLAVOR_SOURCE_DIR.${NC}"
		return
	fi

	mkdir -p "$YAZI_FLAVORS_DIR"

	local flavor source_dir target_dir
	for flavor in faz-dark.yazi faz-light.yazi; do
		source_dir="$FAZ_YAZI_FLAVOR_SOURCE_DIR/$flavor"
		target_dir="$YAZI_FLAVORS_DIR/$flavor"

		if [[ ! -d "$source_dir" ]]; then
			echo -e "${YELLOW}Warning: missing bundled Yazi flavor $source_dir.${NC}"
			continue
		fi

		if [[ ! -f "$source_dir/flavor.toml" ]]; then
			echo -e "${YELLOW}Warning: flavor.toml missing in $source_dir.${NC}"
			continue
		fi

		mkdir -p "$target_dir"
		cp "$source_dir/flavor.toml" "$target_dir/flavor.toml"
	done

	echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Refreshed Faz yazi flavors ${NC}(dark + light)${NC}"
}

ensure_kaku_yazi_theme() {
	mkdir -p "$YAZI_CONFIG_DIR"
	local managed_flavor
	managed_flavor="$(current_kaku_yazi_flavor)"

	if [[ ! -f "$YAZI_THEME_FILE" ]] || is_legacy_kaku_yazi_theme_file; then
		cat <<EOF >"$YAZI_THEME_FILE"
#:schema https://yazi-rs.github.io/schemas/theme.json

# Faz manages the [flavor] section below so Yazi matches the current Faz theme.
# Add your own theme overrides in other sections if needed.
$(_faz_yazi_theme_block "$managed_flavor")
EOF
		echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Initialized yazi theme ${NC}(managed Faz flavor: $managed_flavor)${NC}"
		return
	fi

	if grep -Eq '^[[:space:]]*\[flavor\][[:space:]]*$' "$YAZI_THEME_FILE" && ! grep -Fq "$FAZ_YAZI_THEME_MARKER_START" "$YAZI_THEME_FILE"; then
		echo -e "  ${BLUE}•${NC} ${BOLD}Config${NC}      Preserved existing yazi [flavor] section ${NC}(user-managed)${NC}"
		return
	fi

	local tmp_theme
	tmp_theme="$(mktemp "${TMPDIR:-/tmp}/faz-yazi-theme.XXXXXX")"

	awk -v start="$FAZ_YAZI_THEME_MARKER_START" -v end="$FAZ_YAZI_THEME_MARKER_END" '
		index($0, start) { skip = 1; next }
		index($0, end)   { skip = 0; next }
		!skip { print }
	' "$YAZI_THEME_FILE" >"$tmp_theme"

	# Drop trailing blank lines so the managed block doesn't drift down each run.
	awk 'NF { for (i = 0; i < blank; i++) print ""; blank = 0; print; next } { blank++ }' \
		"$tmp_theme" >"${tmp_theme}.trim" && mv "${tmp_theme}.trim" "$tmp_theme"

	{
		cat "$tmp_theme"
		printf '\n'
		_faz_yazi_theme_block "$managed_flavor"
		printf '\n'
	} >"${tmp_theme}.next"

	mv "${tmp_theme}.next" "$YAZI_THEME_FILE"
	rm -f "$tmp_theme"
	echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Updated yazi theme ${NC}(managed Faz flavor: $managed_flavor)${NC}"
}

install_yazi_wrapper() {
	cat <<'EOF' >"$YAZI_WRAPPER_FILE"
#!/bin/bash
set -euo pipefail

YAZI_THEME_FILE="${HOME}/.config/yazi/theme.toml"
MARKER_START="# ===== Faz Yazi Flavor (managed) ====="
MARKER_END="# ===== End Faz Yazi Flavor (managed) ====="
WRAPPER_PATH="${BASH_SOURCE[0]}"
WRAPPER_DIR="$(cd "$(dirname "$WRAPPER_PATH")" && pwd)"

system_kaku_flavor() {
	local flavor="faz-dark"
	if command -v defaults >/dev/null 2>&1; then
		local appearance
		appearance="$(defaults read -g AppleInterfaceStyle 2>/dev/null || true)"
		if [[ "$appearance" != "Dark" ]]; then
			flavor="faz-light"
		fi
	fi
	printf '%s\n' "$flavor"
}

default_kaku_config_path() {
	if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
		printf '%s\n' "${XDG_CONFIG_HOME}/faz/faz.lua"
	else
		printf '%s\n' "${HOME}/.config/faz/faz.lua"
	fi
}

active_kaku_config_path() {
	if [[ -n "${FAZ_CONFIG_FILE:-}" ]]; then
		printf '%s\n' "${FAZ_CONFIG_FILE}"
	else
		default_kaku_config_path
	fi
}

resolve_kaku_flavor_from_config() {
	local config_file="$1"
	local system_flavor
	system_flavor="$(system_kaku_flavor)"

	if [[ -f "$config_file" ]]; then
		local scheme_line
		scheme_line="$(
			awk '
				/^[[:space:]]*--/ { next }
				/^[[:space:]]*config\.color_scheme[[:space:]]*=/ { print; exit }
			' "$config_file"
		)"
		if [[ -n "$scheme_line" ]]; then
			if [[ "$scheme_line" == *"Faz Light"* ]]; then
				printf '%s\n' "faz-light"
				return
			fi
			if [[ "$scheme_line" == *"Faz Dark"* || "$scheme_line" == *"Faz Theme"* ]]; then
				printf '%s\n' "faz-dark"
				return
			fi
			if [[ "$scheme_line" == *"'Auto'"* || "$scheme_line" == *'"Auto"'* ]]; then
				printf '%s\n' "$system_flavor"
				return
			fi
			if [[ "$scheme_line" == *get_appearance* ]]; then
				printf '%s\n' "$system_flavor"
				return
			fi
			printf '%s\n' "faz-dark"
			return
		fi
	fi

	printf '%s\n' "$system_flavor"
}

current_flavor() {
	resolve_kaku_flavor_from_config "$(active_kaku_config_path)"
}

managed_block() {
	local flavor="$1"
	cat <<BLOCK
$MARKER_START
[flavor]
dark = "$flavor"
light = "$flavor"
$MARKER_END
BLOCK
}

ensure_theme() {
	local flavor="$1"
	mkdir -p "$(dirname "$YAZI_THEME_FILE")"

	if [[ ! -f "$YAZI_THEME_FILE" ]]; then
		cat <<BLOCK >"$YAZI_THEME_FILE"
#:schema https://yazi-rs.github.io/schemas/theme.json

# Faz manages the [flavor] section below so Yazi matches the current Faz theme.
$(managed_block "$flavor")
BLOCK
		return
	fi

	if grep -Eq '^[[:space:]]*\[flavor\][[:space:]]*$' "$YAZI_THEME_FILE" && ! grep -Fq "$MARKER_START" "$YAZI_THEME_FILE"; then
		return
	fi

	local tmp_theme
	tmp_theme="$(mktemp "${TMPDIR:-/tmp}/faz-yazi-wrapper.XXXXXX")"
	awk -v start="$MARKER_START" -v end="$MARKER_END" '
		index($0, start) { skip = 1; next }
		index($0, end)   { skip = 0; next }
		!skip { print }
	' "$YAZI_THEME_FILE" >"$tmp_theme"

	# Drop trailing blank lines so the managed block doesn't drift down each run.
	awk 'NF { for (i = 0; i < blank; i++) print ""; blank = 0; print; next } { blank++ }' \
		"$tmp_theme" >"${tmp_theme}.trim" && mv "${tmp_theme}.trim" "$tmp_theme"

	{
		cat "$tmp_theme"
		printf '\n'
		managed_block "$flavor"
		printf '\n'
	} >"${tmp_theme}.next"

	mv "${tmp_theme}.next" "$YAZI_THEME_FILE"
	rm -f "$tmp_theme"
}

# yazi >= 26.5.6 rejects a `$schema` key in its config files; older Faz
# versions wrote one, so rewrite it to the supported `#:schema` comment.
migrate_schema_headers() {
	local file tmp_file
	for file in "$YAZI_THEME_FILE" "${HOME}/.config/yazi/keymap.toml"; do
		[[ -f "$file" && -w "$file" ]] || continue
		grep -Eq '^[[:space:]]*"?\$schema"?[[:space:]]*=' "$file" || continue
		tmp_file="$(mktemp "${TMPDIR:-/tmp}/faz-yazi-schema.XXXXXX")" || continue
		if sed -E \
			-e 's|^[[:space:]]*"?\$schema"?[[:space:]]*=[[:space:]]*"([^"]+)".*$|#:schema \1|' \
			-e '/^[[:space:]]*"?\$schema"?[[:space:]]*=/d' \
			"$file" >"$tmp_file"; then
			mv "$tmp_file" "$file"
		else
			rm -f "$tmp_file"
		fi
	done
}

resolve_real_yazi() {
	local candidate

	# Check PATH first so any package manager (MacPorts, Nix, etc.) is found
	local path_entry
	IFS=':' read -r -a path_entries <<< "${PATH:-}"
	for path_entry in "${path_entries[@]}"; do
		[[ -z "$path_entry" || "$path_entry" == "$WRAPPER_DIR" ]] && continue
		candidate="$path_entry/yazi"
		if [[ -x "$candidate" && "$candidate" != "$WRAPPER_PATH" ]]; then
			printf '%s\n' "$candidate"
			return 0
		fi
	done

	# Fallback to well-known install locations for GUI-launched shells
	# where PATH may be minimal
	for candidate in /opt/homebrew/bin/yazi /usr/local/bin/yazi /opt/local/bin/yazi; do
		if [[ -x "$candidate" && "$candidate" != "$WRAPPER_PATH" ]]; then
			printf '%s\n' "$candidate"
			return 0
		fi
	done

	return 1
}

main() {
	local flavor real_bin
	migrate_schema_headers
	flavor="$(current_flavor)"
	ensure_theme "$flavor"

	if ! real_bin="$(resolve_real_yazi)"; then
		echo "yazi not found. Install it with: brew install yazi" >&2
		exit 127
	fi

	exec "$real_bin" "$@"
}

main "$@"
EOF
	chmod +x "$YAZI_WRAPPER_FILE"
	echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Installed yazi wrapper ${NC}(theme sync before launch)${NC}"
}

# Ensure vendor resources exist
if [[ ! -d "$VENDOR_DIR" ]]; then
	echo -e "${YELLOW}Error: Vendor resources not found in $VENDOR_DIR${NC}"
	exit 1
fi

install_kaku_terminfo() {
	# Skip explicit bootstrap when requested.
	if [[ "${FAZ_SKIP_TERMINFO_BOOTSTRAP:-0}" == "1" ]]; then
		return
	fi

	# If available in system/user databases already, no-op.
	if infocmp faz >/dev/null 2>&1; then
		return
	fi

	local target_dir="$HOME/.terminfo"
	local compiled_entry="$RESOURCES_DIR/terminfo/6b/faz"
	local source_entry=""

	# App bundles include compiled terminfo entries under Resources/terminfo.
	if [[ -f "$compiled_entry" ]]; then
		if mkdir -p "$target_dir/6b" 2>/dev/null && cp "$compiled_entry" "$target_dir/6b/faz" 2>/dev/null; then
			if infocmp faz >/dev/null 2>&1; then
				echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Installed faz terminfo ${NC}(~/.terminfo)${NC}"
				return
			fi
		else
			echo -e "${YELLOW}Warning: could not copy compiled terminfo entry to ~/.terminfo, continuing.${NC}"
		fi
	fi

	# Dev checkout fallback: compile from source terminfo definition.
	for candidate in \
		"$RESOURCES_DIR/../termwiz/data/faz.terminfo" \
		"$SCRIPT_DIR/../../termwiz/data/faz.terminfo"; do
		if [[ -f "$candidate" ]]; then
			source_entry="$candidate"
			break
		fi
	done

	if [[ -n "$source_entry" ]]; then
		if ! command -v tic >/dev/null 2>&1; then
			echo -e "${YELLOW}Warning: tic not found, skipping faz terminfo install.${NC}"
			return
		fi

		mkdir -p "$target_dir"
		if tic -x -o "$target_dir" "$source_entry" >/dev/null 2>&1 && infocmp faz >/dev/null 2>&1; then
			echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Installed faz terminfo ${NC}(~/.terminfo)${NC}"
			return
		fi
	fi

	echo -e "${YELLOW}Warning: failed to install faz terminfo automatically.${NC}"
}

install_kaku_terminfo

echo -e "${BOLD}Setting up Faz Shell Environment${NC}"

# 1. Prepare User Config Directory
mkdir -p "$USER_CONFIG_DIR"
mkdir -p "$USER_CONFIG_DIR/plugins"
mkdir -p "$USER_CONFIG_DIR/bin"

# 2. Optional external tools bootstrap (Homebrew-managed)
if [[ "${FAZ_SKIP_TOOL_BOOTSTRAP:-0}" != "1" ]]; then
	if [[ -f "$TOOL_INSTALL_SCRIPT" ]]; then
		if ! bash "$TOOL_INSTALL_SCRIPT"; then
			echo -e "${YELLOW}Warning: optional CLI tool bootstrap failed.${NC}"
		fi
	else
		echo -e "${YELLOW}Warning: missing tool bootstrap script at $TOOL_INSTALL_SCRIPT${NC}"
	fi
fi

# Validate required plugin directories up front.
# setup_zsh.sh may be run standalone, so provide a clear dependency hint.
for plugin in fast-syntax-highlighting zsh-autosuggestions zsh-completions zsh-z; do
	if [[ ! -d "$VENDOR_DIR/$plugin" ]]; then
		echo -e "${YELLOW}Error: Missing plugin vendor directory: $VENDOR_DIR/$plugin${NC}"
		echo -e "${YELLOW}Hint: Run scripts/download_vendor.sh before setup_zsh.sh.${NC}"
		exit 1
	fi
done

# Remove legacy plugins replaced in this version
if [[ -n "$USER_CONFIG_DIR" ]]; then
	rm -rf "$USER_CONFIG_DIR/plugins/zsh-syntax-highlighting"
fi
# Copy Plugins
cp -R "$VENDOR_DIR/fast-syntax-highlighting" "$USER_CONFIG_DIR/plugins/"
cp -R "$VENDOR_DIR/zsh-autosuggestions" "$USER_CONFIG_DIR/plugins/"
cp -R "$VENDOR_DIR/zsh-completions" "$USER_CONFIG_DIR/plugins/"
cp -R "$VENDOR_DIR/zsh-z" "$USER_CONFIG_DIR/plugins/"
echo -e "  ${GREEN}✓${NC} ${BOLD}Tools${NC}       Installed Zsh plugins ${NC}(~/.config/faz/zsh/plugins)${NC}"

# Copy Starship Config (if not exists)
if [[ ! -f "$STARSHIP_CONFIG" ]]; then
	if [[ -f "$VENDOR_DIR/starship.toml" ]]; then
		starship_dir="$(dirname "$STARSHIP_CONFIG")"
		if [[ -L "$starship_dir" || ( -e "$starship_dir" && ! -w "$starship_dir" ) ]]; then
			echo -e "  ${YELLOW}!${NC} ${BOLD}Config${NC}      Skipped starship.toml ${NC}($starship_dir is read-only or a symlink)${NC}"
		else
			mkdir -p "$starship_dir"
			cp "$VENDOR_DIR/starship.toml" "$STARSHIP_CONFIG"
			echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Initialized starship.toml ${NC}(~/.config/starship.toml)${NC}"
		fi
	fi
fi

# Repair yazi configs written by older Faz versions before touching them.
migrate_yazi_schema_headers

# Initialize Yazi layout config if the user has not created one yet.
if [[ ! -f "$YAZI_CONFIG_FILE" ]]; then
	if [[ -L "$YAZI_CONFIG_DIR" || ( -e "$YAZI_CONFIG_DIR" && ! -w "$YAZI_CONFIG_DIR" ) ]]; then
		echo -e "  ${YELLOW}!${NC} ${BOLD}Config${NC}      Skipped yazi.toml ${NC}($YAZI_CONFIG_DIR is read-only or a symlink)${NC}"
	else
		mkdir -p "$YAZI_CONFIG_DIR"
		cat <<EOF >"$YAZI_CONFIG_FILE"
[mgr]
ratio = [3, 3, 10]

[preview]
max_width = 2000
max_height = 2400

[opener]
edit = [
  { run = "\${EDITOR:-vim} %s", desc = "edit", for = "unix", block = true },
]
EOF
		echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Initialized yazi.toml ${NC}(~/.config/yazi/yazi.toml)${NC}"
	fi
fi

ensure_yazi_preview_size_defaults() {
	if [[ ! -f "$YAZI_CONFIG_FILE" ]]; then
		return
	fi

	local preview_block
	preview_block="$(awk '
		BEGIN { in_preview = 0 }
		/^[[:space:]]*\[preview\][[:space:]]*$/ { in_preview = 1; next }
		/^[[:space:]]*\[[^]]+\][[:space:]]*$/ { in_preview = 0 }
		in_preview { print }
	' "$YAZI_CONFIG_FILE")"

	local has_preview_section=false
	local has_max_width=false
	local has_max_height=false

	if grep -Eq '^[[:space:]]*\[preview\][[:space:]]*$' "$YAZI_CONFIG_FILE"; then
		has_preview_section=true
	fi
	if grep -Eq '^[[:space:]]*max_width[[:space:]]*=' <<<"$preview_block"; then
		has_max_width=true
	fi
	if grep -Eq '^[[:space:]]*max_height[[:space:]]*=' <<<"$preview_block"; then
		has_max_height=true
	fi

	if [[ "$has_preview_section" == "false" ]]; then
		cat <<EOF >>"$YAZI_CONFIG_FILE"

[preview]
max_width = 2000
max_height = 2400
EOF
		echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Added default Yazi preview size ${NC}(2000x2400)${NC}"
		return
	fi

	if [[ "$has_max_width" == "true" ]] && [[ "$has_max_height" == "true" ]]; then
		return
	fi

	local tmp_yazi
	tmp_yazi="$(mktemp "${TMPDIR:-/tmp}/faz-yazi-preview.XXXXXX")"

	awk -v need_width="$has_max_width" -v need_height="$has_max_height" '
		/^[[:space:]]*\[preview\][[:space:]]*$/ {
			print
			if (need_width != "true") {
				print "max_width = 2000"
			}
			if (need_height != "true") {
				print "max_height = 2400"
			}
			next
		}
		{ print }
	' "$YAZI_CONFIG_FILE" >"$tmp_yazi"

	mv "$tmp_yazi" "$YAZI_CONFIG_FILE"
	echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Completed Yazi preview size defaults ${NC}(2000x2400)${NC}"
}

ensure_yazi_preview_size_defaults

ensure_yazi_edit_opener() {
	if [[ ! -f "$YAZI_CONFIG_FILE" ]]; then
		return
	fi

	# Only skip if the user already has an edit opener defined.
	if grep -Eq '^[[:space:]]*edit[[:space:]]*=' "$YAZI_CONFIG_FILE"; then
		return
	fi

	# If [opener] section exists but has no edit entry, append edit under it.
	if grep -Eq '^[[:space:]]*\[opener\][[:space:]]*$' "$YAZI_CONFIG_FILE"; then
		local tmp_yazi
		tmp_yazi="$(mktemp "${TMPDIR:-/tmp}/faz-yazi-edit.XXXXXX")"
		awk '/^[[:space:]]*\[opener\][[:space:]]*$/ {
			print
			print "edit = ["
			print "  { run = \"${EDITOR:-vim} %s\", desc = \"edit\", for = \"unix\", block = true },"
			print "]"
			next
		}
		{ print }' "$YAZI_CONFIG_FILE" >"$tmp_yazi"
		mv "$tmp_yazi" "$YAZI_CONFIG_FILE"
		echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Added default Yazi edit opener under existing [opener] section"
		return
	fi

	cat <<EOF >>"$YAZI_CONFIG_FILE"

[opener]
edit = [
  { run = "\${EDITOR:-vim} %s", desc = "edit", for = "unix", block = true },
]
EOF
	echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Added default Yazi edit opener ${NC}(vim)${NC}"
}

ensure_yazi_edit_opener

# Initialize Yazi keymap tweaks if the user has not created one yet.
if [[ ! -f "$YAZI_KEYMAP_FILE" ]]; then
	mkdir -p "$YAZI_CONFIG_DIR"
	cat <<EOF >"$YAZI_KEYMAP_FILE"
#:schema https://yazi-rs.github.io/schemas/keymap.json

[mgr]
prepend_keymap = [
  { on = "e", run = "open", desc = "Edit or open selected files" },
  { on = "o", run = "open", desc = "Edit or open selected files" },
  { on = "<Enter>", run = "enter", desc = "Enter the child directory" },
]
EOF
	echo -e "  ${GREEN}✓${NC} ${BOLD}Config${NC}      Initialized yazi keymap ${NC}(~/.config/yazi/keymap.toml)${NC}"
fi

sync_kaku_yazi_flavors
ensure_kaku_yazi_theme
install_yazi_wrapper

AUTOSUGGEST_CLI_PROVIDER=""
if command -v kiro-cli >/dev/null 2>&1; then
	AUTOSUGGEST_CLI_PROVIDER="kiro-cli"
elif command -v q >/dev/null 2>&1 && q --version 2>/dev/null | grep -qi 'amazon'; then
	AUTOSUGGEST_CLI_PROVIDER="q"
fi

if [[ -n "$AUTOSUGGEST_CLI_PROVIDER" ]]; then
	AUTOSUGGEST_BLOCK="$(cat <<EOF
# Faz defers autosuggestions to the external provider detected during faz init.
typeset -g _faz_autosuggest_cli_provider="${AUTOSUGGEST_CLI_PROVIDER}"
typeset -g _faz_external_autosuggest_provider=0

if _faz_has_autosuggest_system; then
    _faz_external_autosuggest_provider=1
fi
if [[ -n "\${_faz_autosuggest_cli_provider:-}" ]]; then
    _faz_external_autosuggest_provider=1
fi
EOF
)"
else
	AUTOSUGGEST_BLOCK="$(cat <<'EOF'
typeset -g _faz_autosuggest_cli_provider=""
typeset -g _faz_external_autosuggest_provider=0

if _faz_has_autosuggest_system; then
    _faz_external_autosuggest_provider=1
fi

# Load zsh-autosuggestions only if:
# 1. User config has not loaded it yet (_zsh_autosuggest_start not defined)
# 2. No other autosuggest system is active (to avoid widget wrapping conflicts)
if ! (( ${+functions[_zsh_autosuggest_start]} )) && [[ "${_faz_external_autosuggest_provider:-0}" != "1" ]] && [[ -f "$FAZ_ZSH_DIR/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh" ]]; then
    source "$FAZ_ZSH_DIR/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh"
fi
EOF
)"
fi

# 3. Create/Update Faz Init File (managed by Faz)
if [[ -f "$FAZ_INIT_FILE" ]]; then
    cp "$FAZ_INIT_FILE" "${FAZ_INIT_FILE}.bak"
fi
FAZ_INIT_TMPFILE="${FAZ_INIT_FILE}.tmp.$$"
cat <<EOF >"$FAZ_INIT_TMPFILE"
# Faz Zsh Integration - DO NOT EDIT MANUALLY
# This file is managed by Faz.app. Any changes may be overwritten.

export FAZ_ZSH_DIR="\$HOME/.config/faz/zsh"

# Add Faz managed bin to PATH (faz wrapper and user tools)
export PATH="\$FAZ_ZSH_DIR/bin:\$PATH"

# Which shells count as "inside Faz". TERM_PROGRAM alone is not enough: tmux
# rewrites it to "tmux", so a tmux session started from Faz lost the prompt
# and Smart Tab. FAZ_SESSION is exported in a Faz shell and inherited by
# that tmux server. Any other terminal sets its own TERM_PROGRAM, so a value
# that leaked through (VS Code launched from Faz) is dropped here (#503).
if [[ "\${TERM_PROGRAM:-}" == "Faz" ]]; then
    export FAZ_SESSION=1
elif [[ -z "\${TMUX:-}" ]]; then
    unset FAZ_SESSION
fi
_faz_in_kaku=0
if [[ "\${TERM_PROGRAM:-}" == "Faz" ]] || [[ -n "\${TMUX:-}" && -n "\${FAZ_SESSION:-}" ]]; then
    _faz_in_kaku=1
fi

# Initialize Starship inside Faz by default. The managed file is sourced by
# every zsh so PATH and shared helpers remain available in IDE and system
# terminals. FAZ_PROMPT_EVERYWHERE=1 opts the prompt into all of them, for
# people who use Faz's Starship as their one prompt (#503).
if { (( _faz_in_kaku )) || [[ -n "\${FAZ_PROMPT_EVERYWHERE:-}" ]]; } && command -v starship &> /dev/null; then
    # Cache the full starship init script. Plain \`starship init zsh\` forks
    # starship on every new shell (twice: the stub it prints re-runs
    # \`starship init zsh --print-full-init\`), and that fork+exec is the
    # largest fixed cost of shell startup. The cache key (binary path +
    # mtime) is recorded on the first line, so upgrading or relocating
    # starship refreshes the cache automatically.
    _faz_starship_init_ok=0
    _faz_starship_cache="\$FAZ_ZSH_DIR/cache/starship-init.zsh"
    _faz_starship_key=""
    if zmodload -F zsh/stat b:zstat 2>/dev/null; then
        typeset -a _faz_starship_stat
        if zstat -A _faz_starship_stat +mtime -- "\${commands[starship]}" 2>/dev/null; then
            _faz_starship_key="# \${commands[starship]} \${_faz_starship_stat[1]}"
        fi
        unset _faz_starship_stat
    fi
    if [[ -n "\$_faz_starship_key" && -r "\$_faz_starship_cache" ]]; then
        _faz_starship_cache_key=""
        IFS= read -r _faz_starship_cache_key < "\$_faz_starship_cache" 2>/dev/null
        if [[ "\$_faz_starship_cache_key" == "\$_faz_starship_key" ]]; then
            builtin source "\$_faz_starship_cache"
            _faz_starship_init_ok=1
        fi
        unset _faz_starship_cache_key
    fi
    if (( ! _faz_starship_init_ok )); then
        _faz_starship_init="\$(starship init zsh --print-full-init)"
        if [[ -n "\$_faz_starship_init" ]]; then
            eval "\$_faz_starship_init"
            if [[ -n "\$_faz_starship_key" ]]; then
                # Write via a per-pid temp file and rename so two shells
                # starting concurrently can't interleave into a truncated
                # cache that later shells would keep sourcing.
                command mkdir -p "\${_faz_starship_cache:h}" 2>/dev/null
                if {
                    builtin print -r -- "\$_faz_starship_key"
                    builtin print -r -- "\$_faz_starship_init"
                } >| "\${_faz_starship_cache}.\$\$" 2>/dev/null; then
                    command mv -f "\${_faz_starship_cache}.\$\$" "\$_faz_starship_cache" 2>/dev/null \
                        || command rm -f "\${_faz_starship_cache}.\$\$" 2>/dev/null
                fi
            fi
        else
            # Fall back to the stock two-stage init if --print-full-init
            # unexpectedly produced nothing.
            eval "\$(starship init zsh)"
        fi
        unset _faz_starship_init
    fi
    unset _faz_starship_init_ok _faz_starship_cache _faz_starship_key

    # Faz workaround: Fix Zsh + Starship bug where Ctrl-C prints the literal RPROMPT string.
    # When Zsh receives SIGINT during prompt evaluation, it aborts the command
    # substitution and prints the literal \$(starship...) string. Pre-evaluating
    # the right prompt in precmd avoids this entirely.
    _faz_render_starship_rprompt() {
        command starship prompt --right \
            --terminal-width="\${COLUMNS:-}" \
            --keymap="\${KEYMAP:-}" \
            --status="\${STARSHIP_CMD_STATUS:-}" \
            --pipestatus="\${STARSHIP_PIPE_STATUS[*]:-}" \
            --cmd-duration="\${STARSHIP_DURATION:-}" \
            --jobs="\${STARSHIP_JOBS_COUNT:-0}" 2>/dev/null
    }

    _faz_fix_starship_rprompt() {
        # Check if RPROMPT currently holds a dynamic starship command
        if [[ "\${RPROMPT:-}" == *'\$('*'starship'*'prompt --right'* ]]; then
            # Capture it and save it as our template
            _faz_starship_rprompt_cmd="\$RPROMPT"
        fi

        # If we have a saved starship command template, we should evaluate it.
        # BUT we only overwrite RPROMPT if RPROMPT is exactly what we set it to last time,
        # or if it is the original starship command itself.
        # If the user sets RPROMPT="foo", we leave it alone.
        if [[ -n "\${_faz_starship_rprompt_cmd:-}" ]]; then
            if [[ "\${RPROMPT:-}" == "\${_faz_starship_rprompt_cmd}" ]] || [[ "\${RPROMPT:-}" == "\${_faz_last_injected_rprompt:-}" ]]; then
                local evaled
                if [[ "\${_faz_starship_rprompt_cmd}" == *starship*'prompt --right'* ]]; then
                    evaled="\$(_faz_render_starship_rprompt)"
                else
                    local cmd="\${_faz_starship_rprompt_cmd}"
                    # Avoid zsh pattern parsing here; strip a literal \$(
                    # prefix and trailing ) via slicing instead.
                    if [[ "\${cmd[1]}" == '$' && "\${cmd[2]}" == '(' && "\${cmd[-1]}" == ')' ]]; then
                        cmd="\${cmd[3,-2]}"
                    fi
                    evaled="\$(eval "\$cmd" 2>/dev/null)"
                fi
                RPROMPT="\$evaled"
                _faz_last_injected_rprompt="\$evaled"
            fi
        fi
    }
    if [[ \${precmd_functions[(Ie)_faz_fix_starship_rprompt]} -eq 0 ]]; then
        precmd_functions+=(_faz_fix_starship_rprompt)
    fi
fi

# Enable color output for ls
export CLICOLOR=1
export LSCOLORS="gxfxcxdxbxegedabagacad"

# Smart History Configuration
HISTSIZE="\${HISTSIZE:-50000}"
SAVEHIST="\${SAVEHIST:-50000}"
if [[ -z "\${HISTFILE:-}" ]]; then
    HISTFILE="\${ZDOTDIR:-\$HOME}/.zsh_history"
fi
setopt HIST_IGNORE_ALL_DUPS      # Remove older duplicate when new entry is added
setopt HIST_FIND_NO_DUPS         # Do not display duplicates when searching history
setopt HIST_REDUCE_BLANKS        # Remove blank lines from history
setopt HIST_IGNORE_SPACE         # Skip commands that start with a space
setopt SHARE_HISTORY             # Share history between all sessions
setopt APPEND_HISTORY            # Append history to the history file (no overwriting)
setopt INC_APPEND_HISTORY        # Write each command to history file immediately
setopt EXTENDED_HISTORY          # Include timestamps in saved history

# Set default Zsh options
setopt interactive_comments
bindkey -e

# Prefix history search on Up/Down (e.g. type "curl" then press Up)
# This is shell behavior, not terminal behavior, so Faz configures it here.
# Skip if an external history navigator (atuin, mcfly, etc.) already owns the
# Up key. After "bindkey -e" the emacs default for ^[[A is "up-line-or-history";
# any other value means a third-party tool has already claimed it.
if [[ "\$(bindkey -M emacs '^[[A' 2>/dev/null)" == *"up-line-or-history"* ]]; then
    autoload -U up-line-or-beginning-search down-line-or-beginning-search
    zle -N up-line-or-beginning-search
    zle -N down-line-or-beginning-search
    zmodload zsh/terminfo 2>/dev/null || true
    for _faz_keymap in emacs viins; do
        [[ -n "\${terminfo[kcuu1]:-}" ]] && bindkey -M "\$_faz_keymap" "\${terminfo[kcuu1]}" up-line-or-beginning-search
        [[ -n "\${terminfo[kcud1]:-}" ]] && bindkey -M "\$_faz_keymap" "\${terminfo[kcud1]}" down-line-or-beginning-search
        bindkey -M "\$_faz_keymap" '^[[A' up-line-or-beginning-search
        bindkey -M "\$_faz_keymap" '^[[B' down-line-or-beginning-search
        bindkey -M "\$_faz_keymap" '^[OA' up-line-or-beginning-search
        bindkey -M "\$_faz_keymap" '^[OB' down-line-or-beginning-search
    done
    unset _faz_keymap
fi

# Faz line-selection widgets for modified arrows in prompt editing.
_faz_select_left_char() {
    emulate -L zsh
    if (( ! REGION_ACTIVE )); then
        zle set-mark-command
    fi
    zle backward-char
}
_faz_select_right_char() {
    emulate -L zsh
    if (( ! REGION_ACTIVE )); then
        zle set-mark-command
    fi
    zle forward-char
}
_faz_select_line_start() {
    emulate -L zsh
    if (( ! REGION_ACTIVE )); then
        zle set-mark-command
    fi
    zle beginning-of-line
}
_faz_select_line_end() {
    emulate -L zsh
    if (( ! REGION_ACTIVE )); then
        zle set-mark-command
    fi
    zle end-of-line
}
_faz_has_active_region() {
    emulate -L zsh
    # Require both an active region flag and a non-empty span. Either one can
    # be stale on its own and would cause false-positive kill-region deletes.
    (( REGION_ACTIVE && MARK != CURSOR ))
}
_faz_deactivate_region() {
    emulate -L zsh
    if ! _faz_has_active_region; then
        return 1
    fi
    if (( \${+widgets[deactivate-region]} )); then
        zle deactivate-region
    else
        MARK=\$CURSOR
        REGION_ACTIVE=0
        zle redisplay
    fi
    return 0
}
# Unconditional region deactivation helper (not bound to any key; called from
# _faz_mv_* widgets below). Unlike _faz_deactivate_region this always clears
# REGION_ACTIVE without checking MARK vs CURSOR, ensuring stale region flags
# are removed even when the selection span is empty.
_faz_force_deactivate_region() {
    emulate -L zsh
    (( ! REGION_ACTIVE )) && return
    if (( \${+widgets[deactivate-region]} )); then
        zle deactivate-region
    else
        REGION_ACTIVE=0
        MARK=\$CURSOR
        zle redisplay
    fi
}
# Movement widgets that auto-deactivate any active region before moving.
# The Faz GUI sends ^B/^F/^A/^E when collapsing a selection with a plain or
# Cmd+arrow key; these wrappers ensure zsh clears REGION_ACTIVE in the same
# keystroke, preventing spurious region-extension or stale region highlights.
_faz_mv_backward_char() {
    emulate -L zsh
    _faz_force_deactivate_region
    zle backward-char
}
_faz_mv_forward_char() {
    emulate -L zsh
    _faz_force_deactivate_region
    zle forward-char
}
_faz_mv_beginning_of_line() {
    emulate -L zsh
    _faz_force_deactivate_region
    zle beginning-of-line
}
_faz_mv_end_of_line() {
    emulate -L zsh
    _faz_force_deactivate_region
    zle end-of-line
}
zle -N _faz_mv_backward_char
zle -N _faz_mv_forward_char
zle -N _faz_mv_beginning_of_line
zle -N _faz_mv_end_of_line
zle -N _faz_select_left_char
zle -N _faz_select_right_char
zle -N _faz_select_line_start
zle -N _faz_select_line_end

# Terminal-assisted selection shortcuts (Faz GUI sends these directly).
_faz_cmd_a_select_all() {
    emulate -L zsh
    # Move to beginning first so MARK is anchored there, then extend to end.
    # If set-mark-command were called first, MARK would be at the current cursor
    # position and only the text after it would be selected.
    zle beginning-of-line
    zle set-mark-command
    zle end-of-line
}
_faz_cmd_shift_left() {
    emulate -L zsh
    zle set-mark-command
    zle beginning-of-line
}
_faz_cmd_shift_right() {
    emulate -L zsh
    zle set-mark-command
    zle end-of-line
}
zle -N _faz_cmd_a_select_all
zle -N _faz_cmd_shift_left
zle -N _faz_cmd_shift_right

# Cancel selection without moving cursor (ESC key in Faz GUI).
_faz_cancel_selection() {
    emulate -L zsh
    _faz_force_deactivate_region
}
zle -N _faz_cancel_selection

# Shift+Left/Right: char expand; Shift+Home/End: to line boundary.
bindkey '^[[1;2D' _faz_select_left_char
bindkey '^[[1;2C' _faz_select_right_char
bindkey '^[[1;2H' _faz_select_line_start
bindkey '^[[1;2F' _faz_select_line_end

# Terminal-assisted selection shortcuts (distinct CSI sequences from Faz GUI).
bindkey '^[[990~' _faz_cmd_a_select_all
bindkey '^[[991~' _faz_cmd_shift_left
bindkey '^[[992~' _faz_cmd_shift_right
bindkey '^[[995~' _faz_cancel_selection

# Emacs movement keys wrapped to auto-deactivate any active region.
# ^B/^F/^A/^E are sent by the Faz GUI when collapsing a selection with a
# plain or Cmd+arrow key. Wrapping them (rather than using a custom CSI escape)
# avoids stray characters if the sequence is received in an unexpected context.
bindkey '^B' _faz_mv_backward_char
bindkey '^F' _faz_mv_forward_char
bindkey '^A' _faz_mv_beginning_of_line
bindkey '^E' _faz_mv_end_of_line

# Bind delete keys to native zsh widgets. The Faz GUI handles selection-aware
# deletion directly (sending kill sequences via line_editor_selection), so the
# shell side does not need a wrapper here.
bindkey '^?' backward-delete-char
bindkey '^H' backward-delete-char
bindkey '^[[3~' delete-char
# Cmd+Backspace sends ^U via the default Faz key binding. zsh emacs mode
# defaults ^U to kill-whole-line, which also deletes text after the cursor;
# backward-kill-line matches macOS/readline delete-to-line-start behavior.
bindkey '^U' backward-kill-line
bindkey '^G' send-break

# Directory Navigation Options
setopt auto_cd
setopt auto_pushd
setopt pushd_ignore_dups
setopt pushdminus

# Common Aliases (Intuitive defaults)
alias ll='ls -lhF'   # Detailed list (human-readable sizes, no hidden files)
alias la='ls -lAhF'  # List all (including hidden, except . and ..)
alias l='ls -CF'     # Compact list

# Directory Navigation
alias ...='../..'
alias ....='../../..'
alias .....='../../../..'
alias ......='../../../../..'

alias md='mkdir -p'
alias rd=rmdir

# Grep Colors
alias grep='grep --color=auto'
alias egrep='grep -E --color=auto'
alias fgrep='grep -F --color=auto'

# Common Git Aliases (The Essentials)
alias g='git'
alias ga='git add'
alias gaa='git add --all'
alias gb='git branch'
alias gbd='git branch -d'
alias gc='git commit -v'
alias gcmsg='git commit -m'
alias gco='git checkout'
alias gcb='git checkout -b'
alias gd='git diff'
alias gds='git diff --staged'
alias gf='git fetch'
alias gl='git pull'
alias gp='git push'
alias gst='git status'
alias gss='git status -s'
alias glo='git log --oneline --decorate'
alias glg='git log --stat'
alias glgp='git log --stat -p'

# yazi launcher - cd into the directory yazi is in when you exit.
'y'() {
    emulate -L zsh
    setopt local_options no_sh_word_split

    local yazi_cmd="\$FAZ_ZSH_DIR/bin/yazi"
    if [[ ! -x "\$yazi_cmd" ]]; then
        yazi_cmd="\$(command -v yazi 2>/dev/null || true)"
    fi

    if [[ -z "\$yazi_cmd" ]]; then
        echo "yazi not found. Install it with: brew install yazi"
        return 127
    fi

    local tmp cwd
    tmp="\$(mktemp -t 'yazi-cwd.XXXXXX')"
    "\$yazi_cmd" "\$@" --cwd-file="\$tmp"
    if cwd="\$(command cat -- "\$tmp")" && [[ -n "\$cwd" && "\$cwd" != "\$PWD" ]]; then
        builtin cd -- "\$cwd"
    fi
    rm -f -- "\$tmp"
}

# k - AI chat CLI bundled with Faz.
'k'() {
    emulate -L zsh
    local k_cmd
    for _candidate in \
        "\${FAZ_ZSH_DIR:+\$FAZ_ZSH_DIR/../../MacOS/k}" \
        "\$HOME/Applications/Faz.app/Contents/MacOS/k" \
        "/Applications/Faz.app/Contents/MacOS/k"; do
        if [[ -x "\$_candidate" ]]; then
            k_cmd="\$_candidate"
            break
        fi
    done
    if [[ -z "\$k_cmd" ]]; then
        echo "k: Faz app not found. Install Faz from https://github.com/tw93/Faz"
        return 127
    fi
    "\$k_cmd" "\$@"
}

# Load Plugins (Performance Optimized)

# Load zsh-completions into fpath before compinit.
# If the user already added this path, do not duplicate it.
if [[ -d "\$FAZ_ZSH_DIR/plugins/zsh-completions/src" ]] && (( \${fpath[(Ie)\$FAZ_ZSH_DIR/plugins/zsh-completions/src]} == 0 )); then
    fpath=("\$FAZ_ZSH_DIR/plugins/zsh-completions/src" \$fpath)
fi

# Optimized compinit:
# - If completion system is already initialized by user config/plugin manager, skip.
# - Otherwise use cache and only rebuild when needed.
autoload -Uz compinit
if ! (( \${+functions[_main_complete]} )) || ! (( \${+_comps} )); then
    if [[ -n "\${ZDOTDIR:-\$HOME}/.zcompdump"(#qN.mh+24) ]]; then
        # Rebuild completion cache if older than 24 hours
        compinit
    else
        # Load from cache (much faster)
        compinit -C
    fi
fi

_faz_has_jump_provider() {
    (( \${+functions[z]} )) \
        || (( \${+functions[zshz]} )) \
        || (( \${+functions[__zoxide_z]} )) \
        || (( \${+functions[_zoxide_z]} ))
}

# Load zsh-z (smart directory jumping) if not already provided by user config.
if [[ -f "\$FAZ_ZSH_DIR/plugins/zsh-z/zsh-z.plugin.zsh" ]] && ! _faz_has_jump_provider; then
    # Default to smart case matching so \`z kaku\` prefers \`Faz\` over lowercase
    # path entries. Users can still override this in their own shell config.
    : "\${ZSHZ_CASE:=smart}"
    export ZSHZ_CASE
    source "\$FAZ_ZSH_DIR/plugins/zsh-z/zsh-z.plugin.zsh"
fi
unset -f _faz_has_jump_provider 2>/dev/null

# cd + Tab falls back to zsh-z frecency history when filesystem completion
# has no match. Delegate ranking to zshz --complete so behavior stays aligned
# with the plugin (frecency ordering, smart-case, future plugin changes).
if (( \${+functions[zshz]} )); then
    _faz_cd_history_complete() {
        emulate -L zsh
        setopt extended_glob no_sh_word_split

        _cd
        local ret=\$?
        local nmatches="\${compstate[nmatches]:-0}"
        if (( nmatches > 0 )); then
            return \$ret
        fi

        local token="\${PREFIX:-}"
        [[ -n "\$token" ]] || return \$ret
        [[ "\$token" != -* ]] || return \$ret

        (( \${+functions[zshz]} )) || return \$ret

        local -a matches
        local match
        while IFS= read -r match; do
            [[ -n "\$match" ]] || continue
            matches+=("\$match")
        done < <(zshz --complete -- "\$token" 2>/dev/null)

        (( \${#matches[@]} )) || return \$ret

        compadd -Q -U -X "zsh-z history dirs" -- "\${matches[@]}"
        return 0
    }

    if (( \${+functions[compdef]} )); then
        compdef _faz_cd_history_complete cd
    fi
fi

# Detect if any autosuggest system is already active (e.g., Kiro CLI, Fig, etc.)
# These systems wrap zle widgets with names containing "autosuggest", which would
# conflict with zsh-autosuggestions and cause FUNCNEST recursion errors.
_faz_has_autosuggest_system() {
    local w
    for w in \${(k)widgets}; do
        case "\${w:l}" in
            autosuggest-*) continue ;;  # zsh-autosuggestions' own widgets
            *autosuggest*) return 0 ;;  # third-party (Kiro CLI, Fig, etc.)
        esac
    done
    return 1
}

$AUTOSUGGEST_BLOCK
unset -f _faz_has_autosuggest_system 2>/dev/null

# Smart Tab behavior:
# - Use completion while typing arguments/path-like tokens
# - When Faz sets FAZ_TAB_ACCEPT_SUGGEST_FIRST=1, accept a visible
#   autosuggestion before falling back to completion
# - Without FAZ_TAB_ACCEPT_SUGGEST_FIRST, prefer completion so Tab reveals
#   candidates instead of accepting recent-history suggestions
# - Only claim Tab inside Faz sessions unless explicitly disabled
if [[ -z "\${FAZ_SMART_TAB_DISABLE:-}" ]] && (( _faz_in_kaku )); then
    _faz_tab_widget() {
        emulate -L zsh

        local has_suggestion=0
        local prefer_suggestion_first=0

        if [[ "\${FAZ_TAB_ACCEPT_SUGGEST_FIRST:-0}" == "1" ]]; then
            prefer_suggestion_first=1
        fi

        # When Faz defers autosuggestions to an external provider, keep Tab
        # as completion-only to avoid widget recursion.
        if [[ "\${_faz_external_autosuggest_provider:-0}" != "1" ]] && (( \${+widgets[autosuggest-accept]} )) && [[ -n "\${POSTDISPLAY:-}" ]]; then
            has_suggestion=1
        fi

        # Use completion while typing arguments (e.g. 'vim READ<Tab>')
        # and for path-like command tokens ('./scr<Tab>').
        local lbuf="\${LBUFFER}"
        local trimmed="\${lbuf#\${lbuf%%[![:space:]]*}}"
        local current_token="\${lbuf##*[[:space:]]}"

        if [[ -z "\$trimmed" || "\$trimmed" == *[[:space:]]* || "\$current_token" == */* ]]; then
            zle expand-or-complete
            return
        fi

        if (( has_suggestion )) && (( prefer_suggestion_first )); then
            zle autosuggest-accept
        else
            zle expand-or-complete
        fi
    }
    zle -N _faz_tab_widget
    bindkey '^I' _faz_tab_widget
fi

# Defer fast-syntax-highlighting to first prompt (~40ms saved at startup)
# This plugin must be loaded LAST, and we delay it for faster shell startup.
# If user config already loaded it, skip to avoid overriding user settings.
if ! (( \${+functions[_zsh_highlight]} )) && [[ -f "\$FAZ_ZSH_DIR/plugins/fast-syntax-highlighting/fast-syntax-highlighting.plugin.zsh" ]]; then
    # Defer loading until first prompt display
    fast_syntax_highlighting_defer() {
        source "\$FAZ_ZSH_DIR/plugins/fast-syntax-highlighting/fast-syntax-highlighting.plugin.zsh"

        # Override comment color: fsh default (fg=8) is invisible on dark backgrounds.
        typeset -gA FAST_HIGHLIGHT_STYLES
        FAST_HIGHLIGHT_STYLES[comment]='fg=249'

        # Drop the underline on existing-directory paths: fsh default for
        # path-to-dir is fg=magenta,underline, and that underline stacks with
        # Faz's hyperlink hover underline into a confusing double line. Keep
        # the magenta color so a valid directory still reads as a path.
        FAST_HIGHLIGHT_STYLES[path-to-dir]='fg=magenta'

        # Remove this hook after first run
        precmd_functions=("\${precmd_functions[@]:#fast_syntax_highlighting_defer}")
    }

    # Hook into precmd (runs before prompt is displayed)
    precmd_functions+=(fast_syntax_highlighting_defer)
fi

# Recover the shell after a TUI or SSH session exits without restoring input
# modes (#551). Run at the next prompt, never on wake while a TUI is still alive.
_faz_reset_mouse_tracking() {
    if [[ "\${TERM_PROGRAM:-}" == "Faz" || ( -n "\${TMUX:-}" && -n "\${FAZ_SESSION:-}" ) ]]; then
        printf '\033[?1000;1002;1003;1004;1005;1006;1007;1016l'
    fi
    return 0
}
if [[ \${precmd_functions[(Ie)_faz_reset_mouse_tracking]} -eq 0 ]]; then
    precmd_functions+=(_faz_reset_mouse_tracking)
fi


# Auto-set TERM to xterm-256color for SSH connections when running under faz,
# since remote hosts typically lack the faz terminfo entry.
# Also auto-detect 1Password SSH agent and add IdentitiesOnly=yes to prevent
# "Too many authentication failures" caused by 1Password offering all stored keys.
# Set FAZ_SSH_SKIP_1PASSWORD_FIX=1 to disable the 1Password behavior.
# Guard: only define if no existing ssh function is present, so user-defined
# wrappers (e.g. from fzf-ssh, autossh plugins) are not silently replaced.
# The wrapper body must stay self-contained: agent snapshot tools (Claude
# Code and similar) capture the ssh function into shell snapshots but drop
# _-prefixed helpers, so calling one from here leaves a dangling reference
# in snapshot-restored shells (#493).
if (( \$+aliases[ssh] )); then
    typeset _faz_existing_ssh_alias="\${aliases[ssh]}"
    function ssh {
        local -a _faz_alias_words _faz_ssh_cmd _faz_ssh_args
        _faz_alias_words=(\${(z)_faz_existing_ssh_alias})
        # Snapshot-restored shells keep this function but not the alias
        # variable; fall back to plain ssh instead of exec'ing "\$1".
        if [[ \${#_faz_alias_words[@]} -eq 0 ]]; then
            _faz_ssh_cmd=(command ssh)
            _faz_ssh_args=("\$@")
        elif [[ "\${_faz_alias_words[1]-}" == "ssh" ]]; then
            _faz_ssh_cmd=(command ssh)
            _faz_ssh_args=("\${(@)_faz_alias_words[2,-1]}" "\$@")
        elif [[ "\${_faz_alias_words[1]-}" == "command" && "\${_faz_alias_words[2]-}" == "ssh" ]]; then
            _faz_ssh_cmd=(command ssh)
            _faz_ssh_args=("\${(@)_faz_alias_words[3,-1]}" "\$@")
        elif [[ "\${_faz_alias_words[1]-}" == *=* ]]; then
            # Alias starts with VAR=value prefixes (e.g. alias ssh='TERM=xterm ssh');
            # expanded array words are not re-parsed as assignments, so route
            # through env to keep them out of command position.
            _faz_ssh_cmd=(env "\${_faz_alias_words[@]}")
            _faz_ssh_args=("\$@")
        else
            _faz_ssh_cmd=("\${_faz_alias_words[@]}")
            _faz_ssh_args=("\$@")
        fi

        local -a extra_opts=()
        if [[ -z "\${FAZ_SSH_SKIP_1PASSWORD_FIX-}" ]]; then
            local sock="\${SSH_AUTH_SOCK:-}"
            if [[ "\$sock" == *1password* || "\$sock" == *2BUA8C4S2C* ]]; then
                local has_identitiesonly=false prev="" arg
                for arg in "\${_faz_ssh_args[@]}"; do
                    [[ "\$prev" == "-o" && "\$arg" == IdentitiesOnly=* ]] && has_identitiesonly=true
                    [[ "\$arg" == -oIdentitiesOnly=* ]] && has_identitiesonly=true
                    prev="\$arg"
                done
                \$has_identitiesonly || extra_opts+=(-o "IdentitiesOnly=yes")
            fi
        fi

        if [[ -z "\${FAZ_SSH_SKIP_TERM_FIX-}" && "\$TERM" == "faz" ]]; then
            TERM=xterm-256color "\${_faz_ssh_cmd[@]}" "\${extra_opts[@]}" "\${_faz_ssh_args[@]}"
        else
            "\${_faz_ssh_cmd[@]}" "\${extra_opts[@]}" "\${_faz_ssh_args[@]}"
        fi
    }
    unalias ssh
elif ! typeset -f ssh > /dev/null 2>&1; then
function ssh {
    local -a extra_opts=()
    if [[ -z "\${FAZ_SSH_SKIP_1PASSWORD_FIX-}" ]]; then
        local sock="\${SSH_AUTH_SOCK:-}"
        if [[ "\$sock" == *1password* || "\$sock" == *2BUA8C4S2C* ]]; then
            local has_identitiesonly=false prev="" arg
            for arg in "\$@"; do
                [[ "\$prev" == "-o" && "\$arg" == IdentitiesOnly=* ]] && has_identitiesonly=true
                [[ "\$arg" == -oIdentitiesOnly=* ]] && has_identitiesonly=true
                prev="\$arg"
            done
            \$has_identitiesonly || extra_opts+=(-o "IdentitiesOnly=yes")
        fi
    fi
    if [[ -z "\${FAZ_SSH_SKIP_TERM_FIX-}" && "\$TERM" == "faz" ]]; then
        TERM=xterm-256color command ssh "\${extra_opts[@]}" "\$@"
    else
        command ssh "\${extra_opts[@]}" "\$@"
    fi
}
fi

# Same TERM fix for mosh: mosh-server inherits TERM on the remote side, so a
# faz TERM breaks remote rendering exactly like plain ssh would. Guard: keep
# user-defined mosh functions and aliases untouched. Self-contained (#493).
if command -v mosh > /dev/null 2>&1 && ! typeset -f mosh > /dev/null 2>&1 && ! (( \$+aliases[mosh] )); then
function mosh {
    if [[ -z "\${FAZ_SSH_SKIP_TERM_FIX-}" && "\$TERM" == "faz" ]]; then
        TERM=xterm-256color command mosh "\$@"
    else
        command mosh "\$@"
    fi
}
fi

# Auto-set TERM to xterm-256color for sudo commands when running under faz.
# sudo usually resets TERMINFO_DIRS, so root processes (e.g. nano) can fail
# with "unknown terminal type 'faz'" even though Faz set TERMINFO_DIRS for the
# user shell. Set FAZ_SUDO_SKIP_TERM_FIX=1 to disable this behavior.
# Guard: only define if no existing sudo function is present.
# If sudo is an alias, zsh expands it during function-definition parsing and
# raises a syntax error ("defining function based on alias"). Unalias first.
if ! typeset -f sudo > /dev/null 2>&1; then
unalias sudo 2>/dev/null || true
function sudo {
    if [[ -z "\${FAZ_SUDO_SKIP_TERM_FIX-}" && "\$TERM" == "faz" ]]; then
        TERM=xterm-256color command sudo "\$@"
    else
        command sudo "\$@"
    fi
}
fi

# Faz Dark maps ANSI 8 / bright_black to #3A3942, which makes any text rendered
# at fg=8 (the default comment color in fast-syntax-highlighting and
# zsh-syntax-highlighting) invisible. The deferred loader blocks above already
# override the comment color when Faz itself loaded the plugin, but they are
# skipped when the user pre-loaded their own copy in .zshrc (oh-my-zsh, brew,
# etc.). This one-shot precmd guard reapplies the override after .zshrc has
# fully run, only when the comment style is still at an invisible default, so
# users who picked their own color are preserved.
_faz_apply_highlight_styles() {
    # Both fast-syntax-highlighting and zsh-syntax-highlighting ship the same
    # invisible default for \`[comment]\`: fg=black,bold (older versions: fg=8).
    # Faz Dark's color_overrides collapse those to #3A3942 against #1F1D2C,
    # so the # character and any zsh-style # comment becomes unreadable.
    # Replace ONLY the known defaults; leave any other value alone so a user
    # who picked their own comment color in .zshrc keeps it.
    if (( \${+FAST_HIGHLIGHT_STYLES} )); then
        case "\${FAST_HIGHLIGHT_STYLES[comment]:-}" in
            ''|fg=8|fg=black|fg=black,bold|fg=8,bold|8|black)
                FAST_HIGHLIGHT_STYLES[comment]='fg=249' ;;
        esac
    fi
    if (( \${+ZSH_HIGHLIGHT_STYLES} )); then
        case "\${ZSH_HIGHLIGHT_STYLES[comment]:-}" in
            ''|fg=8|fg=black|fg=black,bold|fg=8,bold|8|black)
                ZSH_HIGHLIGHT_STYLES[comment]='fg=249' ;;
        esac
    fi
    precmd_functions=("\${precmd_functions[@]:#_faz_apply_highlight_styles}")
}
precmd_functions+=(_faz_apply_highlight_styles)
EOF

if [[ -s "$FAZ_INIT_TMPFILE" ]]; then
    mv "$FAZ_INIT_TMPFILE" "$FAZ_INIT_FILE"
    # Byte-compile the (~1600 line) init file so each new shell skips the
    # parse. zsh's `source` automatically prefers faz.zsh.zwc while it is
    # not older than faz.zsh, and falls back to the source file otherwise,
    # so a failed or skipped zcompile is harmless.
    zsh -fc 'zcompile -R -- "$1"' zcompile "$FAZ_INIT_FILE" 2>/dev/null || rm -f "${FAZ_INIT_FILE}.zwc"
else
    echo -e "  ${RED}✗${NC} Generated faz.zsh is empty, keeping previous version" >&2
    rm -f "$FAZ_INIT_TMPFILE"
fi

echo -e "  ${GREEN}✓${NC} ${BOLD}Script${NC}      Generated faz.zsh init script"

# 4. Configure tmux (Optional)
TMUX_SOURCE_LINE='source-file "$HOME/.config/faz/tmux/faz.tmux.conf" # Faz tmux Integration'

write_kaku_tmux_file() {
	mkdir -p "$FAZ_TMUX_DIR"
	cat <<'EOF' >"$FAZ_TMUX_FILE"
# Faz tmux Integration - DO NOT EDIT MANUALLY
# This file is managed by Faz.app. Any changes may be overwritten.

set -g mouse on
# Panes opened after attaching from Faz keep the Faz prompt (#503)
set -ga update-environment FAZ_SESSION
bind-key -n S-WheelUpPane if-shell -F '#{pane_in_mode}' 'send-keys -X -N 5 scroll-up' 'copy-mode -e -u'
bind-key -n S-WheelDownPane if-shell -F '#{pane_in_mode}' 'send-keys -X -N 5 scroll-down' ''
EOF
	echo -e "  ${GREEN}✓${NC} ${BOLD}Script${NC}      Generated managed tmux integration"
}

normalize_kaku_tmux_source_line() {
	if [[ ! -f "$TMUXRC" ]]; then
		return
	fi

	local tmp_file
	tmp_file="$(mktemp "${TMPDIR:-/tmp}/faz-tmuxrc.XXXXXX")"

	if awk -v source_line="$TMUX_SOURCE_LINE" '
BEGIN { replaced = 0; extra = 0 }
{
	if ($0 ~ /^[[:space:]]*#/ ) {
		print
		next
	}

	if ($0 ~ /^[[:space:]]*source-file[[:space:]]+/ &&
	    $0 ~ /(kaku|faz)\/tmux\/(kaku|faz)\.tmux\.conf/) {
		if (!replaced) {
			print source_line
			replaced = 1
		} else {
			extra++
		}
		next
	}

	print
}
END {
	if (replaced && extra > 0) {
		exit 2
	} else if (replaced) {
		exit 0
	}
	exit 3
}
' "$TMUXRC" >"$tmp_file"; then
		if ! cmp -s "$TMUXRC" "$tmp_file"; then
			backup_tmuxrc_once
			mv "$tmp_file" "$TMUXRC"
			echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Updated Faz source line in .tmux.conf"
		else
			rm -f "$tmp_file"
		fi
	else
		local awk_status="$?"
		if [[ "$awk_status" == "2" ]]; then
			if ! cmp -s "$TMUXRC" "$tmp_file"; then
				backup_tmuxrc_once
				mv "$tmp_file" "$TMUXRC"
				echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Removed duplicate Faz source line(s) from .tmux.conf"
			else
				rm -f "$tmp_file"
			fi
		else
			rm -f "$tmp_file"
			if [[ "$awk_status" != "3" ]]; then
				echo -e "${YELLOW}Warning: failed to normalize Faz source line in .tmux.conf; leaving it unchanged.${NC}"
			fi
		fi
	fi
}

has_kaku_tmux_source_line() {
	if [[ ! -f "$TMUXRC" ]]; then
		return 1
	fi

	if grep -Fqx "$TMUX_SOURCE_LINE" "$TMUXRC"; then
		return 0
	fi

	grep -Eq '^[[:space:]]*source-file[[:space:]].*(kaku|faz)/tmux/(kaku|faz)\.tmux\.conf([[:space:]]|$)' "$TMUXRC"
}

ensure_kaku_tmux_integration() {
	# GUI-launched shells inherit a minimal PATH (no Homebrew/MacPorts). Probe
	# common install locations so tmux is found even when PATH is stripped down.
	local tmux_cmd=""
	if command -v tmux >/dev/null 2>&1; then
		tmux_cmd="tmux"
	elif [[ -x /opt/homebrew/bin/tmux ]]; then
		tmux_cmd=/opt/homebrew/bin/tmux
	elif [[ -x /usr/local/bin/tmux ]]; then
		tmux_cmd=/usr/local/bin/tmux
	elif [[ -x /opt/local/bin/tmux ]]; then
		tmux_cmd=/opt/local/bin/tmux
	fi

	if [[ -z "$tmux_cmd" ]]; then
		echo -e "  ${BLUE}•${NC} ${BOLD}Integrate${NC}   Skipped tmux integration ${NC}(tmux not found)${NC}"
		return
	fi

	write_kaku_tmux_file
	normalize_kaku_tmux_source_line

	if has_kaku_tmux_source_line; then
		echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Already linked in .tmux.conf"
	else
		backup_tmuxrc_once
		if [[ -f "$TMUXRC" && -s "$TMUXRC" ]]; then
			echo "" >>"$TMUXRC"
		fi
		echo "$TMUX_SOURCE_LINE" >>"$TMUXRC"
		echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Successfully patched .tmux.conf"
	fi
}

ensure_kaku_tmux_integration

# 5. Configure .zshrc
PATH_LINE='[[ ":$PATH:" != *":$HOME/.config/faz/zsh/bin:"* ]] && export PATH="$HOME/.config/faz/zsh/bin:$PATH" # Faz PATH Integration'
SOURCE_LINE='[[ -f "$HOME/.config/faz/zsh/faz.zsh" ]] && source "$HOME/.config/faz/zsh/faz.zsh" # Faz Shell Integration'
LEGACY_INLINE_BLOCK_PRESERVED=0

# SYNC: the heredoc below must stay in sync with FAZ_LEGACY_INLINE_KNOWN_LINES
# in faz-cli/src/reset.rs. When adding or removing lines, update both places.
legacy_inline_block_has_only_kaku_managed_lines() {
	local line

	for line in "$@"; do
		if [[ -z "${line//[[:space:]]/}" ]]; then
			continue
		fi

		if ! grep -Fqx -- "$line" <<'EOF'
# Faz Zsh Integration - DO NOT EDIT MANUALLY
# This file is managed by Faz.app. Any changes may be overwritten.
export FAZ_ZSH_DIR="$HOME/.config/faz/zsh"
# Add bundled binaries to PATH
export PATH="$FAZ_ZSH_DIR/bin:$PATH"
# Initialize Starship (Cross-shell prompt)
# Check file existence to avoid "no such file" errors in some zsh configurations
if [[ -x "$FAZ_ZSH_DIR/bin/starship" ]]; then
    eval "$("$FAZ_ZSH_DIR/bin/starship" init zsh)"
elif command -v starship &> /dev/null; then
    # Fallback to system starship if available
    eval "$(starship init zsh)"
fi
# Enable color output for ls
export CLICOLOR=1
export LSCOLORS="Gxfxcxdxbxegedabagacad"
# Smart History Configuration
HISTSIZE=50000
SAVEHIST=50000
HISTFILE="$HOME/.zsh_history"
setopt HIST_IGNORE_DUPS          # Do not record an event that was just recorded again
setopt HIST_IGNORE_SPACE         # Do not record an event starting with a space
setopt HIST_FIND_NO_DUPS         # Do not display a line previously found
setopt SHARE_HISTORY             # Share history between all sessions
setopt APPEND_HISTORY            # Append history to the history file (no overwriting)
# Set default Zsh options
setopt interactive_comments
bindkey -e
# Directory Navigation Options
setopt auto_cd
setopt auto_pushd
setopt pushd_ignore_dups
setopt pushdminus
# Common Aliases (Intuitive defaults)
alias ll='ls -lhF'   # Detailed list (human-readable sizes, no hidden files)
alias la='ls -lAhF'  # List all (including hidden, except . and ..)
alias l='ls -CF'     # Compact list
# Directory Navigation
alias ...='../..'
alias ....='../../..'
alias .....='../../../..'
alias ......='../../../../..'
alias md='mkdir -p'
alias rd=rmdir
# Grep Colors
alias grep='grep --color=auto'
alias egrep='grep -E --color=auto'
alias fgrep='grep -F --color=auto'
# Common Git Aliases (The Essentials)
alias g='git'
alias ga='git add'
alias gaa='git add --all'
alias gb='git branch'
alias gbd='git branch -d'
alias gc='git commit -v'
alias gcmsg='git commit -m'
alias gco='git checkout'
alias gcb='git checkout -b'
alias gd='git diff'
alias gds='git diff --staged'
alias gf='git fetch'
alias gl='git pull'
alias gp='git push'
alias gst='git status'
alias gss='git status -s'
alias glo='git log --oneline --decorate'
alias glg='git log --stat'
alias glgp='git log --stat -p'
# Load Plugins (Performance Optimized)
# Load zsh-completions into fpath before compinit
if [[ -d "$FAZ_ZSH_DIR/plugins/zsh-completions/src" ]]; then
    fpath=("$FAZ_ZSH_DIR/plugins/zsh-completions/src" $fpath)
fi
# Optimized compinit: Use cache and only rebuild when needed (~30ms saved)
autoload -Uz compinit
if [[ -n "${ZDOTDIR:-$HOME}/.zcompdump"(#qN.mh+24) ]]; then
    # Rebuild completion cache if older than 24 hours
    compinit
else
    # Load from cache (much faster)
    compinit -C
fi
# Load zsh-z (smart directory jumping) - Fast, no delay needed
if [[ -f "$FAZ_ZSH_DIR/plugins/zsh-z/zsh-z.plugin.zsh" ]]; then
    # Default to smart case matching so `z kaku` prefers `Faz` over lowercase
    # path entries. Users can still override this in their own shell config.
    : "${ZSHZ_CASE:=smart}"
    export ZSHZ_CASE
    source "$FAZ_ZSH_DIR/plugins/zsh-z/zsh-z.plugin.zsh"
fi
# Load zsh-autosuggestions only if:
# 1. User config has not loaded it yet (_zsh_autosuggest_start not defined)
# 2. No other autosuggest system is active (to avoid widget wrapping conflicts)
if ! (( ${+functions[_zsh_autosuggest_start]} )) && [[ "${_faz_external_autosuggest_provider:-0}" != "1" ]] && [[ -f "$FAZ_ZSH_DIR/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh" ]]; then
    source "$FAZ_ZSH_DIR/plugins/zsh-autosuggestions/zsh-autosuggestions.zsh"
    # Smart Tab: suggestion-first when FAZ_TAB_ACCEPT_SUGGEST_FIRST=1,
    # otherwise completion-first.
    # Keep this widget out of autosuggestions rebinding, otherwise POSTDISPLAY is
    # cleared before our condition check and Tab always falls back to completion.
    typeset -ga ZSH_AUTOSUGGEST_IGNORE_WIDGETS
    ZSH_AUTOSUGGEST_IGNORE_WIDGETS+=(_faz_tab_accept_or_complete)
    _faz_tab_accept_or_complete() {
        if [[ "${FAZ_TAB_ACCEPT_SUGGEST_FIRST:-0}" == "1" ]] && [[ -n "$POSTDISPLAY" ]]; then
            zle autosuggest-accept
        else
            zle expand-or-complete
        fi
    }
    zle -N _faz_tab_accept_or_complete
    bindkey -M emacs '^I' _faz_tab_accept_or_complete
    bindkey -M main '^I' _faz_tab_accept_or_complete
    bindkey -M viins '^I' _faz_tab_accept_or_complete
fi
# Defer zsh-syntax-highlighting to first prompt (~40ms saved at startup)
# This plugin must be loaded LAST, and we delay it for faster shell startup
source "$FAZ_ZSH_DIR/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh"
if [[ -f "$FAZ_ZSH_DIR/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh" ]]; then
    # Simplified highlighters for better performance (removed brackets, pattern, cursor)
    export ZSH_HIGHLIGHT_HIGHLIGHTERS=(main)
    # Defer loading until first prompt display
    zsh_syntax_highlighting_defer() {
        source "$FAZ_ZSH_DIR/plugins/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh"
        # Override comment color: default (fg=8) is invisible on dark backgrounds.
        ZSH_HIGHLIGHT_STYLES[comment]='fg=249'
        # Remove this hook after first run
        precmd_functions=("${precmd_functions[@]:#zsh_syntax_highlighting_defer}")
    }
    # Hook into precmd (runs before prompt is displayed)
    precmd_functions+=(zsh_syntax_highlighting_defer)
fi
EOF
		then
			return 1
		fi
	done

	return 0
}

# Migrate legacy inline block from older versions to the single source-line model.
cleanup_legacy_inline_block() {
	if [[ ! -f "$ZSHRC" ]]; then
		return
	fi

	if ! grep -q "^# Faz Shell Integration$" "$ZSHRC"; then
		return
	fi

	if ! grep -q "zsh-syntax-highlighting/zsh-syntax-highlighting.zsh" "$ZSHRC"; then
		return
	fi

	if ! grep -q "FAZ_ZSH_DIR" "$ZSHRC"; then
		return
	fi

	local tmp_file
	local line
	local -a block_lines=()
	local in_block=0
	local saw_kaku_var=0
	local saw_syntax=0
	local removed_block=0
	local preserved_block=0
	local skip_blank_after_removed=0
	tmp_file="$(mktemp "${TMPDIR:-/tmp}/faz-zshrc.XXXXXX")"

	while IFS= read -r line || [[ -n "$line" ]]; do
		if [[ "$in_block" == "0" ]]; then
			if [[ "$skip_blank_after_removed" == "1" && -z "${line//[[:space:]]/}" ]]; then
				continue
			fi
			skip_blank_after_removed=0

			if [[ "$line" == "# Faz Shell Integration" ]]; then
				in_block=1
				saw_kaku_var=0
				saw_syntax=0
				block_lines=()
				continue
			fi

			printf '%s\n' "$line" >>"$tmp_file"
			continue
		fi

		block_lines+=("$line")
		[[ "$line" == *FAZ_ZSH_DIR* ]] && saw_kaku_var=1
		[[ "$line" == *zsh-syntax-highlighting/zsh-syntax-highlighting.zsh* ]] && saw_syntax=1

		if [[ "$saw_kaku_var" == "1" && "$saw_syntax" == "1" && "$line" =~ ^[[:space:]]*fi[[:space:]]*$ ]]; then
			if legacy_inline_block_has_only_kaku_managed_lines "${block_lines[@]}"; then
				removed_block=1
				skip_blank_after_removed=1
			else
				preserved_block=1
				printf '%s\n' "# Faz Shell Integration" >>"$tmp_file"
				local block_line
				for block_line in "${block_lines[@]}"; do
					printf '%s\n' "$block_line" >>"$tmp_file"
				done
			fi

			in_block=0
			saw_kaku_var=0
			saw_syntax=0
			block_lines=()
		fi
	done <"$ZSHRC"

	if [[ "$in_block" == "1" ]]; then
		rm -f "$tmp_file"
		LEGACY_INLINE_BLOCK_PRESERVED=1
		echo -e "${YELLOW}Warning: found unterminated legacy Faz block; leaving .zshrc unchanged.${NC}"
		return
	fi

	if ! cmp -s "$ZSHRC" "$tmp_file"; then
		backup_zshrc_once
		mv "$tmp_file" "$ZSHRC"
		if [[ "$removed_block" == "1" ]]; then
			echo -e "  ${GREEN}✓${NC} ${BOLD}Migrate${NC}     Removed legacy inline Faz block from .zshrc"
		fi
		if [[ "$preserved_block" == "1" ]]; then
			LEGACY_INLINE_BLOCK_PRESERVED=1
			echo -e "${YELLOW}Warning: kept legacy Faz block with custom lines to avoid deleting user shell config.${NC}"
		fi
	else
		rm -f "$tmp_file"
		if [[ "$preserved_block" == "1" ]]; then
			LEGACY_INLINE_BLOCK_PRESERVED=1
			echo -e "${YELLOW}Warning: kept legacy Faz block with custom lines to avoid deleting user shell config.${NC}"
		fi
	fi
}

cleanup_legacy_inline_block

normalize_kaku_path_line() {
	if [[ ! -f "$ZSHRC" ]]; then
		return
	fi

	local tmp_file
	tmp_file="$(mktemp "${TMPDIR:-/tmp}/faz-zshrc.XXXXXX")"

	# Exit codes: 0 = replaced exactly 1 line, 2 = collapsed duplicates, 3 = no match.
	# Only normalize Faz's single-line PATH guard variants; leave user-managed
	# multi-line or custom PATH logic untouched.
	if awk -v path_line="$PATH_LINE" '
BEGIN { replaced = 0; extra = 0 }
{
	if ($0 ~ /^[[:space:]]*#/ ) {
		print
		next
	}

	if ($0 ~ /^[[:space:]]*\[\[/ &&
	    $0 ~ /(kaku|faz)\/zsh\/bin/ &&
	    $0 ~ /&&[[:space:]]*export[[:space:]]+PATH=/) {
		if (!replaced) {
			print path_line
			replaced = 1
		} else {
			extra++
		}
		next
	}

	print
}
END {
	if (replaced && extra > 0) {
		exit 2
	} else if (replaced) {
		exit 0
	}
	exit 3
}
' "$ZSHRC" >"$tmp_file"; then
		if ! cmp -s "$ZSHRC" "$tmp_file"; then
			backup_zshrc_once
			mv "$tmp_file" "$ZSHRC"
			echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Updated Faz PATH line in .zshrc"
		else
			rm -f "$tmp_file"
		fi
	else
		local awk_status="$?"
		if [[ "$awk_status" == "2" ]]; then
			if ! cmp -s "$ZSHRC" "$tmp_file"; then
				backup_zshrc_once
				mv "$tmp_file" "$ZSHRC"
				echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Removed duplicate Faz PATH line(s) from .zshrc"
			else
				rm -f "$tmp_file"
			fi
		else
			rm -f "$tmp_file"
			if [[ "$awk_status" != "3" ]]; then
				echo -e "${YELLOW}Warning: failed to normalize Faz PATH line in .zshrc; leaving it unchanged.${NC}"
			fi
		fi
	fi
}

normalize_kaku_path_line

normalize_faz_source_line() {
	if [[ ! -f "$ZSHRC" ]]; then
		return
	fi

	local tmp_file
	tmp_file="$(mktemp "${TMPDIR:-/tmp}/faz-zshrc.XXXXXX")"

	# Exit codes: 0 = replaced exactly 1 line, 2 = collapsed duplicates, 3 = no match.
	if awk -v source_line="$SOURCE_LINE" '
BEGIN { replaced = 0; extra = 0 }
{
	if ($0 ~ /^[[:space:]]*#/ ) {
		print
		next
	}

	if ($0 ~ /^[[:space:]]*\[\[/ &&
	    $0 ~ /(kaku|faz)\/zsh\/(kaku|faz)\.zsh/ &&
	    $0 ~ /&&[[:space:]]*source[[:space:]]/) {
		if (!replaced) {
			print source_line
			replaced = 1
		} else {
			extra++
		}
		next
	}

	print
}
END {
	if (replaced && extra > 0) {
		exit 2
	} else if (replaced) {
		exit 0
	}
	exit 3
}
' "$ZSHRC" >"$tmp_file"; then
		if ! cmp -s "$ZSHRC" "$tmp_file"; then
			backup_zshrc_once
			mv "$tmp_file" "$ZSHRC"
			echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Updated Faz source line in .zshrc"
		else
			rm -f "$tmp_file"
		fi
	else
		# Capture $? before any `local` declaration; zsh resets $? to 0 on `local`.
		local awk_status="$?"
		if [[ "$awk_status" == "2" ]]; then
			if ! cmp -s "$ZSHRC" "$tmp_file"; then
				backup_zshrc_once
				mv "$tmp_file" "$ZSHRC"
				echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Removed duplicate Faz source line(s) from .zshrc"
			else
				rm -f "$tmp_file"
			fi
		else
			rm -f "$tmp_file"
			if [[ "$awk_status" != "3" ]]; then
				echo -e "${YELLOW}Warning: failed to normalize Faz source line in .zshrc; leaving it unchanged.${NC}"
			fi
		fi
	fi
}

normalize_faz_source_line

has_kaku_path_line() {
	if [[ ! -f "$ZSHRC" ]]; then
		return 1
	fi

	if grep -Fqx "$PATH_LINE" "$ZSHRC"; then
		return 0
	fi

	grep -Eq '^[[:space:]]*\[\[.*(kaku|faz)/zsh/bin.*\]\][[:space:]]*&&[[:space:]]*export[[:space:]]+PATH=.*(kaku|faz)/zsh/bin' "$ZSHRC"
}

has_kaku_source_line() {
	if [[ ! -f "$ZSHRC" ]]; then
		return 1
	fi

	# Prefer exact match for the managed line to avoid false positives from comments.
	if grep -Fqx "$SOURCE_LINE" "$ZSHRC"; then
		return 0
	fi

	# Fallback: accept equivalent active source lines while avoiding comment-only matches.
	grep -Eq '^[[:space:]]*\[\[.*(kaku|faz)/zsh/(kaku|faz)\.zsh.*\]\][[:space:]]*&&[[:space:]]*source[[:space:]].*(kaku|faz)/zsh/(kaku|faz)\.zsh([[:space:]]|$)' "$ZSHRC"
}

# Check if the managed lines already exist
if has_kaku_path_line && has_kaku_source_line; then
	echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Already linked in .zshrc"
elif [[ "$LEGACY_INLINE_BLOCK_PRESERVED" == "1" ]]; then
	echo -e "  ${BLUE}•${NC} ${BOLD}Integrate${NC}   Preserved legacy inline Faz block ${NC}(move custom lines outside it, then rerun faz init)${NC}"
else
	if [[ -f "$ZSHRC" && ! -w "$ZSHRC" ]]; then
		echo -e "  ${YELLOW}!${NC} ${BOLD}Integrate${NC}   .zshrc is read-only (symlink or permission). Add manually:"
		echo -e "              $PATH_LINE"
		echo -e "              $SOURCE_LINE"
	else
		# Backup existing .zshrc only if it doesn't have Faz logic yet
		backup_zshrc_once

		if [[ -f "$ZSHRC" && -s "$ZSHRC" ]]; then
			echo "" >>"$ZSHRC"
		fi
		if ! has_kaku_path_line; then
			echo "$PATH_LINE" >>"$ZSHRC"
		fi
		if ! has_kaku_source_line; then
			echo "$SOURCE_LINE" >>"$ZSHRC"
		fi
		echo -e "  ${GREEN}✓${NC} ${BOLD}Integrate${NC}   Successfully patched .zshrc"
	fi
fi

# 6. Configure TouchID for Sudo (Optional)
# Reference: logic from www/mole/bin/touchid.sh
configure_touchid() {
	PAM_SUDO_FILE="/etc/pam.d/sudo"
	PAM_SUDO_LOCAL_FILE="/etc/pam.d/sudo_local"
	PAM_TID_LINE="auth       sufficient     pam_tid.so"

	# 1. Check if already enabled
	if grep -q "pam_tid.so" "$PAM_SUDO_LOCAL_FILE" 2>/dev/null || grep -q "pam_tid.so" "$PAM_SUDO_FILE" 2>/dev/null; then
		return 0
	fi

	# 2. Check compatibility (Apple Silicon or Intel Macs with TouchID)
	if ! command -v bioutil &>/dev/null; then
		# Fallback check for arm64
		if [[ "$(uname -m)" != "arm64" ]]; then
			return 0
		fi
	fi

	echo -en "\n${BOLD}TouchID for sudo${NC}  Enable fingerprint authentication? (Y/n) "
	read -p "" -n 1 -r
	# Default to Yes (proceed if reply is empty or y/Y)
	if [[ -n "$REPLY" && ! $REPLY =~ ^[Yy]$ ]]; then
		echo "" # Clear the line after Skip
		return 0
	fi
	echo "" # Move to next line for result display

	# Try the modern sudo_local method (macOS Sonoma+)
	if grep -q "sudo_local" "$PAM_SUDO_FILE" 2>/dev/null; then
		echo "# sudo_local: local customizations for sudo" | sudo tee "$PAM_SUDO_LOCAL_FILE" >/dev/null
		echo "$PAM_TID_LINE" | sudo tee -a "$PAM_SUDO_LOCAL_FILE" >/dev/null
		sudo chmod 444 "$PAM_SUDO_LOCAL_FILE"
		sudo chown root:wheel "$PAM_SUDO_LOCAL_FILE"
		echo -e "  ${GREEN}✓${NC} ${BOLD}Sudo${NC}        Enabled via sudo_local"
	else
		# Fallback to editing /etc/pam.d/sudo
		sudo awk -v line="$PAM_TID_LINE" 'NR==2{print line} 1' "$PAM_SUDO_FILE" >"${PAM_SUDO_FILE}.tmp" &&
			sudo mv "${PAM_SUDO_FILE}.tmp" "$PAM_SUDO_FILE"
		echo -e "  ${GREEN}✓${NC} ${BOLD}Sudo${NC}        Enabled via /etc/pam.d/sudo"
	fi
}

if [[ "$UPDATE_ONLY" != "true" ]]; then
	configure_touchid
fi
