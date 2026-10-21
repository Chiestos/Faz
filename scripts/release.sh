#!/usr/bin/env bash
set -euo pipefail

# Release script for Faz
# Usage: ./scripts/release.sh
#
# Prerequisites:
#   - Clean git working tree on main branch
#   - gh CLI authenticated (for creating releases)
#   - Apple Developer ID certificate in login Keychain (or set FAZ_SIGNING_IDENTITY)
#   - Notarization credentials via rcodesign API key or notarytool Keychain profile
#
# Environment variables:
#   FAZ_SIGNING_IDENTITY    - Signing identity (auto-detected from Keychain if not set)
#   FAZ_ASC_API_KEY_PATH    - rcodesign App Store Connect API key JSON path
#   FAZ_NOTARYTOOL_PROFILE  - notarytool Keychain profile name
#   RUN_CLIPPY               - Set to 1 to also run clippy (default: 0)
#   SKIP_TESTS               - Set to 1 to skip tests (default: 0)
#
# Resume flags (skip earlier stages after a mid-release failure):
#   --notarize-only  Skip build; notarize existing dist/Faz.app, then upload.
#   --upload-only    Skip build + notarize; upload existing dist/Faz.dmg.
#
# Dry-run flag (validates pre-flight, then exits without building or publishing):
#   --dry-run        Run all pre-flight checks (git, version, gh auth, signing
#                    identity, notarization creds, release notes, config) and
#                    exit before make check / cargo build / codesign / notary /
#                    git push / gh release create. Equivalent to DRY_RUN=1.

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# shellcheck source=lib/preflight.sh
source "$REPO_ROOT/scripts/lib/preflight.sh"

APP_NAME="Faz"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/dist}"
PROFILE="${PROFILE:-release-opt}"
BUILD_ARCH="${BUILD_ARCH:-arm64}"
RUN_CLIPPY="${RUN_CLIPPY:-0}"
SKIP_TESTS="${SKIP_TESTS:-0}"
GITHUB_REPO="${GITHUB_REPO:-tw93/Faz}"
DRY_RUN="${DRY_RUN:-0}"

# Resume stage flags.
SKIP_BUILD=0
SKIP_NOTARIZE=0
SKIP_UPLOAD=0

for arg in "$@"; do
    case "$arg" in
        --notarize-only) SKIP_BUILD=1 ;;
        --upload-only)   SKIP_BUILD=1; SKIP_NOTARIZE=1 ;;
        --dry-run)       DRY_RUN=1 ;;
    esac
done

is_valid_team_id() {
    [[ "$1" =~ ^[A-Z0-9]{10}$ ]]
}

is_developer_id_application_identity() {
    [[ "$1" == Developer\ ID\ Application:* ]]
}

# Run a stage with timing output.
time_stage() {
    local name="$1"
    shift
    log_info "[stage:$name] starting"
    local t0 t1
    t0=$(date +%s)
    "$@"
    t1=$(date +%s)
    log_info "[stage:$name] done in $((t1 - t0))s"
}

# Verify version consistency across crates
check_version_consistency() {
    log_info "Checking version consistency..."
    local faz_version faz_gui_version
    faz_version=$(grep '^version =' "$REPO_ROOT/faz-cli/Cargo.toml" | head -n1 | cut -d'"' -f2)
    faz_gui_version=$(grep '^version =' "$REPO_ROOT/faz-gui/Cargo.toml" | head -n1 | cut -d'"' -f2)

    if [[ "$faz_version" != "$faz_gui_version" ]]; then
        die "Version mismatch: faz=$faz_version, faz-gui=$faz_gui_version"
    fi

    log_info "Version: $faz_version"
}

# Check release notes match version
check_release_notes() {
    log_info "Checking release notes..."
    if [[ -x "$REPO_ROOT/scripts/check_release_notes.sh" ]]; then
        "$REPO_ROOT/scripts/check_release_notes.sh"
    else
        log_warn "check_release_notes.sh not found or not executable"
    fi
}

# Check config release metadata is ready
check_release_config() {
    log_info "Checking config release metadata..."
    if [[ ! -x "$REPO_ROOT/scripts/check_release_config.sh" ]]; then
        die "scripts/check_release_config.sh is missing or not executable"
    fi

    "$REPO_ROOT/scripts/check_release_config.sh"
}

# Release-shaped builds (app bundle assembly) are validated by
# the Build Validation workflow, which only runs on build-pipeline changes,
# daily, or on dispatch — not on every push. Require its latest run on main to
# be green before starting the local release ritual, so a broken x86_64 build
# or bundle script surfaces here instead of 20 minutes into the release.
check_build_validation() {
    log_info "Checking latest Build Validation workflow run..."

    local line
    line=$(gh run list \
        --workflow 'Build Validation' \
        --branch main \
        --limit 1 \
        --json conclusion,url \
        --jq '.[0] | "\(.conclusion)|\(.url)"' 2>/dev/null || true)

    if [[ -z "$line" || "$line" == "null|null" ]]; then
        log_warn "No Build Validation run found on main."
        log_warn "Dispatch one with: gh workflow run 'Build Validation'"
        return 0
    fi

    local conclusion="${line%%|*}"
    local url="${line##*|}"

    case "$conclusion" in
        success)
            log_info "Build Validation green: $url"
            ;;
        "" | null)
            log_warn "Build Validation run still in progress: $url"
            log_warn "The local release build will validate the same surface; continuing."
            ;;
        *)
            die "Latest Build Validation run is '$conclusion' ($url). Fix or rerun it before releasing."
            ;;
    esac
}

extract_release_title() {
    local release_notes_file="$REPO_ROOT/.github/RELEASE_NOTES.md"
    local title

    if [[ ! -f "$release_notes_file" ]]; then
        return 1
    fi

    title=$(awk '/^# / { sub(/^# /, ""); print; exit }' "$release_notes_file")
    if [[ -z "$title" ]]; then
        return 1
    fi

    printf '%s\n' "$title"
}

# Check gh CLI is authenticated
check_gh_auth() {
    log_info "Checking GitHub CLI authentication..."
    if ! command -v gh >/dev/null 2>&1; then
        die "gh CLI not found. Install from https://cli.github.com/"
    fi

    if ! gh auth status >/dev/null 2>&1; then
        die "gh CLI not authenticated. Run: gh auth login"
    fi
}

# Detect Developer ID from Keychain if not set
detect_signing_identity() {
    if [[ -n "${FAZ_SIGNING_IDENTITY:-}" ]]; then
        if ! is_developer_id_application_identity "$FAZ_SIGNING_IDENTITY"; then
            die "FAZ_SIGNING_IDENTITY must be a Developer ID Application certificate, got: $FAZ_SIGNING_IDENTITY"
        fi
        log_info "Using signing identity from environment: $FAZ_SIGNING_IDENTITY"
        return 0
    fi

    log_info "Detecting signing identity from Keychain..."

    # Find Developer ID Application certificates
    local identities
    identities=$(security find-identity -v -p codesigning 2>/dev/null | grep "Developer ID Application" | awk -F '"' '{print $2}' || true)

    local count
    count=$(echo "$identities" | grep -c "^Developer ID Application" || true)

    if [[ "$count" -eq 0 ]]; then
        die "No Developer ID Application certificate found in Keychain.\n" \
            "Install your certificate or set FAZ_SIGNING_IDENTITY environment variable."
    fi

    FAZ_SIGNING_IDENTITY=$(echo "$identities" | grep "^Developer ID Application" | head -n1)
    export FAZ_SIGNING_IDENTITY
    if [[ "$count" -gt 1 ]]; then
        log_warn "Multiple Developer ID Application certificates found, auto-selecting the first match"
    fi
    log_info "Auto-detected signing identity: $FAZ_SIGNING_IDENTITY"
}

validate_release_profile() {
    case "$PROFILE" in
        release|release-opt)
            log_info "Using release build profile: $PROFILE"
            ;;
        *)
            die "Invalid PROFILE=$PROFILE for release flow. Use PROFILE=release or PROFILE=release-opt."
            ;;
    esac
}

# Check notarization credentials are available
check_notarization_creds() {
    log_info "Checking notarization credentials..."

    local have_creds=0
    local asc_api_key_path notarytool_profile

    asc_api_key_path="${FAZ_ASC_API_KEY_PATH:-}"
    if [[ -z "$asc_api_key_path" ]]; then
        asc_api_key_path=$(security find-generic-password -s "faz-asc-api-key-path" -w 2>/dev/null || true)
    fi
    if [[ -n "$asc_api_key_path" && -f "$asc_api_key_path" ]] && command -v rcodesign >/dev/null 2>&1; then
        have_creds=1
        log_info "Found App Store Connect API key for rcodesign notarization"
    fi

    notarytool_profile="${FAZ_NOTARYTOOL_PROFILE:-}"
    if [[ -z "$notarytool_profile" ]]; then
        notarytool_profile=$(security find-generic-password -s "faz-notarytool-profile" -w 2>/dev/null || true)
    fi
    if [[ -n "$notarytool_profile" ]]; then
        have_creds=1
        log_info "Found notarytool Keychain profile"
    fi

    if [[ "$have_creds" -eq 0 ]]; then
        log_warn "Notarization credentials not found"
        log_warn "Notarization may fail. To set up credentials:"
        log_warn "  security add-generic-password -s 'faz-asc-api-key-path' -a "faz" -w '/path/to/asc_api_key.json'"
        log_warn ""
        log_warn "Or store a notarytool Keychain profile:"
        log_warn "  xcrun notarytool store-credentials faz-notarytool --apple-id <apple-id> --team-id <team-id>"
        log_warn "  security add-generic-password -s 'faz-notarytool-profile' -a "faz" -w 'faz-notarytool'"
        if [[ ! -t 0 ]]; then
            die "Notarization credentials are missing and stdin is not interactive."
        fi
        read -r -p "Continue anyway? [y/N] " response
        if [[ ! "$response" =~ ^[Yy]$ ]]; then
            exit 1
        fi
    fi
}

# Run all quality checks
run_checks() {
    log_info "Running format check..."
    make fmt-check

    log_info "Running compilation check..."
    make check

    if [[ "$RUN_CLIPPY" == "1" ]]; then
        log_info "Running clippy..."
        cargo clippy --locked --all-targets -- -D warnings
    fi

    if [[ "$SKIP_TESTS" == "0" ]]; then
        log_info "Running tests..."
        make test
    else
        log_warn "Skipping tests (SKIP_TESTS=1)"
    fi
}

# Build the release
build_release() {
    log_info "Building release (PROFILE=$PROFILE, ARCH=$BUILD_ARCH)..."

    export FAZ_SIGNING_IDENTITY
    export FAZ_REQUIRE_SIGNED_RELEASE=1
    export PROFILE
    export BUILD_ARCH
    export OUT_DIR

    ./scripts/build.sh
}

# Notarize the release
notarize_release() {
    log_info "Submitting for notarization..."
    ./scripts/notarize.sh
}

# Create and push git tag
create_tag() {
    local version="$1"
    local tag="V${version}"
    local head_sha
    local tag_sha
    local remote_tag_sha

    log_info "Creating tag $tag..."
    head_sha=$(git rev-parse HEAD)

    if git show-ref --verify --quiet "refs/tags/$tag"; then
        tag_sha=$(git rev-parse "$tag^{}")
        if [[ "$tag_sha" != "$head_sha" ]]; then
            die "Tag $tag already exists at $tag_sha, but HEAD is $head_sha."
        fi

        log_warn "Tag $tag already exists at current HEAD, reusing it."
    else
        git tag -a "$tag" -m "Release $tag"
    fi

    remote_tag_sha=$(git ls-remote --tags origin "refs/tags/${tag}^{}" | awk 'NR == 1 { print $1 }')
    if [[ -n "$remote_tag_sha" ]]; then
        if [[ "$remote_tag_sha" != "$head_sha" ]]; then
            die "Origin already has tag $tag at $remote_tag_sha, but HEAD is $head_sha."
        fi

        log_warn "Origin already has tag $tag at current HEAD, skipping push."
        return 0
    fi

    log_info "Pushing tag $tag..."
    git push origin "$tag"
}

# Create GitHub Release
create_github_release() {
    local version="$1"
    local tag="V${version}"
    local release_notes_file="$REPO_ROOT/.github/RELEASE_NOTES.md"
    local release_title="$APP_NAME $tag"
    local notes_arg=""
    local release_edit_args=()
    local release_title_from_notes=""

    # Build a cleaned notes file: strip the first heading line and remove blank
    # lines between numbered list items so GitHub doesn't render extra spacing.
    local notes_tmp
    notes_tmp=$(mktemp /tmp/faz-release-notes.XXXXXX.md)
    # shellcheck disable=SC2064
    trap "rm -f $notes_tmp" RETURN

    if [[ -f "$release_notes_file" ]]; then
        # Skip leading "# Title" line (and following blank line), then collapse
        # blank lines that appear between numbered list items.
        awk '
            NR == 1 && /^# / { next }
            NR == 2 && /^[[:space:]]*$/ { next }
            /^[[:space:]]*$/ { blank=1; next }
            blank { if (!/^[0-9]+\./) printf "\n"; blank=0 }
            { print }
            END { if (blank) printf "\n" }
        ' "$release_notes_file" > "$notes_tmp"

        if [[ -s "$notes_tmp" ]]; then
            notes_arg="--notes-file"
        else
            notes_arg="--generate-notes"
        fi
    else
        notes_arg="--generate-notes"
    fi

    if release_title_from_notes=$(extract_release_title); then
        release_title="$release_title_from_notes"
    fi

    log_info "Creating GitHub Release for $tag..."

    if [[ "$notes_arg" == "--notes-file" ]]; then
        release_edit_args=(--title "$release_title" "$notes_arg" "$notes_tmp")
    else
        release_edit_args=(--title "$release_title")
    fi

    # Check if release already exists
    if gh release view "$tag" -R "$GITHUB_REPO" >/dev/null 2>&1; then
        log_warn "Release $tag already exists, reconciling title, notes, and assets..."
        gh release edit "$tag" \
            -R "$GITHUB_REPO" \
            "${release_edit_args[@]}"
        gh release upload "$tag" \
            -R "$GITHUB_REPO" \
            "$OUT_DIR/Faz.dmg" \
            "$OUT_DIR/faz_for_update.zip" \
            "$OUT_DIR/faz_for_update.zip.sha256" \
            --clobber
    else
        if [[ "$notes_arg" == "--notes-file" ]]; then
            gh release create "$tag" \
                -R "$GITHUB_REPO" \
                "$OUT_DIR/Faz.dmg" \
                "$OUT_DIR/faz_for_update.zip" \
                "$OUT_DIR/faz_for_update.zip.sha256" \
                --title "$release_title" \
                "$notes_arg" "$notes_tmp"
        else
            gh release create "$tag" \
                -R "$GITHUB_REPO" \
                "$OUT_DIR/Faz.dmg" \
                "$OUT_DIR/faz_for_update.zip" \
                "$OUT_DIR/faz_for_update.zip.sha256" \
                --title "$release_title" \
                --generate-notes
        fi
    fi

    log_info "GitHub Release created: https://github.com/${GITHUB_REPO}/releases/tag/$tag"
}


# Main release flow
main() {
    local version

    log_info "Starting release process for $APP_NAME..."

    # Get version
    version=$(get_cargo_version)
    log_info "Releasing version: $version"

    # Always-on pre-flight (cheap and catches misconfiguration on resume too)
    check_clean_git
    check_version_consistency
    check_gh_auth

    if [[ "$SKIP_BUILD" -eq 0 ]]; then
        check_release_notes
        check_release_config
        check_build_validation
        validate_release_profile
        detect_signing_identity
        check_notarization_creds
    fi

    if [[ "$DRY_RUN" == "1" ]]; then
        log_warn "[DRY-RUN] All pre-flight checks passed."
        log_warn "[DRY-RUN] Would build → notarize → tag V${version} → upload"
        log_warn "[DRY-RUN] Expected artifacts: $OUT_DIR/Faz.dmg, $OUT_DIR/faz_for_update.zip, $OUT_DIR/faz_for_update.zip.sha256"
        log_warn "[DRY-RUN] GitHub repo: $GITHUB_REPO"
        log_warn "[DRY-RUN] Unset DRY_RUN (or drop --dry-run) to run for real."
        return 0
    fi

    if [[ "$SKIP_BUILD" -eq 0 ]]; then
        time_stage "checks" run_checks
        time_stage "build" build_release
    else
        log_warn "Skipping build (resume flag)"
        if [[ ! -f "$OUT_DIR/Faz.dmg" ]]; then
            die "Resume requires prior build; $OUT_DIR/Faz.dmg not found."
        fi
        detect_signing_identity
    fi

    if [[ "$SKIP_NOTARIZE" -eq 0 ]]; then
        check_notarization_creds
        time_stage "notarize" notarize_release
    else
        log_warn "Skipping notarize (resume flag)"
    fi

    if [[ "$SKIP_UPLOAD" -eq 0 ]]; then
        time_stage "tag" create_tag "$version"
        time_stage "upload" create_github_release "$version"
    else
        log_warn "Skipping upload (resume flag)"
    fi

    log_info "Release $version complete!"
    log_info "Artifacts:"
    log_info "  - $OUT_DIR/Faz.dmg"
    log_info "  - $OUT_DIR/faz_for_update.zip"
    log_info "  - $OUT_DIR/faz_for_update.zip.sha256"
    log_info ""
    log_info "GitHub Release: https://github.com/${GITHUB_REPO}/releases/tag/V${version}"
}

main "$@"
