#!/bin/bash
# Reset Faz First Run Experience
# This script is for testing purposes. It removes persisted Faz state
# so that Faz will trigger the first run setup again.

set -e

CONFIG_DIR="$HOME/.config/faz"
STATE_FILE="$CONFIG_DIR/state.json"
LEGACY_FILES=(
	"$CONFIG_DIR/.first_run_completed"
	"$CONFIG_DIR/.faz_config_version"
	"$CONFIG_DIR/.faz_window_geometry"
	"$CONFIG_DIR/.faz_window_position"
)

echo "Resetting Faz First Run..."

if [[ -f "$STATE_FILE" ]]; then
	rm "$STATE_FILE"
	echo "✅ Removed state file: $STATE_FILE"
else
	echo "ℹ️  State file not found: $STATE_FILE"
fi

for file in "${LEGACY_FILES[@]}"; do
	if [[ -f "$file" ]]; then
		rm "$file"
		echo "✅ Removed legacy file: $file"
	fi
done

echo "Now relaunch Faz to see the First Run experience."
