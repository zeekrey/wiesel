#!/usr/bin/env bash
# Shared by build-app.sh and dev.sh; compatible with macOS Bash 3.2.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
app="$root/dist/Wiesel.app"
lock="$root/dist/.build-lock"
stage=""
backup=""
installed=0

fail() { printf 'Error: %s\n' "$*" >&2; return 1; }

cleanup() {
    # Restore the previous bundle if installation was interrupted after moving it.
    if [[ -n "$backup" && -d "$backup" && "$installed" == 0 ]]; then
        if [[ -e "$app" ]] || ! mv "$backup" "$app"; then
            printf 'Previous bundle retained at %s; recover it manually.\n' "$backup" >&2
            rmdir "$lock" 2>/dev/null || true
            return
        fi
    fi
    [[ -z "$stage" ]] || rm -rf "$stage"
    rmdir "$lock" 2>/dev/null || true
}

prepare() {
    profile=debug
    case "${1:-}" in
        '') ;;
        --release) profile=release ;;
        *) fail "Usage: bash scripts/${mode}.sh [--release]"; return 1 ;;
    esac
    [[ $# -le 1 ]] || { fail 'Too many arguments'; return 1; }
    # Optional trusted local shell configuration; environment takes precedence.
    local identity_override="${WIESEL_SIGNING_IDENTITY-}"
    if [[ -f "$root/.wiesel-dev.env" ]]; then source "$root/.wiesel-dev.env"; fi
    if [[ -n "$identity_override" ]]; then WIESEL_SIGNING_IDENTITY="$identity_override"; fi
    identity="${WIESEL_SIGNING_IDENTITY:--}"
    if [[ "$mode" == dev && "$identity" == '-' ]]; then
        fail 'dev.sh requires WIESEL_SIGNING_IDENTITY (a persistent code-signing certificate). See README → Development builds.'
        return 1
    fi
    if [[ "$identity" != '-' ]]; then
        # Resolve a name or fingerprint to exactly one currently valid identity.
        local identities matches count
        identities="$(security find-identity -v -p codesigning)"
        matches="$(printf '%s\n' "$identities" | awk -v wanted="$identity" '
            $1 ~ /^[0-9]+\)$/ && length($2) == 40 {
                name = $0; sub(/^[^"]*"/, "", name); sub(/"[^"]*$/, "", name)
                if (toupper($2) == toupper(wanted) || name == wanted) print $2
            }')"
        count="$(printf '%s\n' "$matches" | awk 'NF { n++ } END { print n+0 }')"
        [[ "$count" == 1 ]] || { fail "Signing identity '$identity' must match exactly one valid certificate with a private key. Run: security find-identity -v -p codesigning"; return 1; }
        identity="$matches"
    else
        printf 'Warning: ad-hoc signing; Accessibility permission may need re-granting.\n' >&2
    fi
    mkdir -p "$root/dist"
    mkdir "$lock" 2>/dev/null || { fail "Another build is active ($lock). If a previous build was killed, remove this empty lock directory manually."; return 1; }
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    stage="$(mktemp -d "$root/dist/.stage.XXXXXX")"
    cd "$root"
}

stage_bundle() {
    if [[ "$profile" == release ]]; then cargo build --release --locked; else cargo build --locked; fi
    mkdir -p "$stage/Wiesel.app/Contents/MacOS"
    cp "$root/target/$profile/wiesel" "$stage/Wiesel.app/Contents/MacOS/Wiesel"
    cp "$root/resources/Info.plist" "$stage/Wiesel.app/Contents/Info.plist"
    codesign --force --sign "$identity" "$stage/Wiesel.app"
    codesign --verify --strict "$stage/Wiesel.app"
}

app_pids() {
    # comm (not command/args) matches the full executable path, including spaces.
    ps -ww -axo pid=,comm= | awk -v executable="$app/Contents/MacOS/Wiesel" '
        { pid=$1; sub(/^[[:space:]]*[0-9]+[[:space:]]+/, ""); if ($0 == executable) print pid }'
}

stop_app() {
    local pid pids current attempt
    pids="$(app_pids)"
    for pid in $pids; do
        # Recheck the path just before signalling; never kill by app name alone.
        current="$(app_pids)"
        if grep -qx "$pid" <<< "$current"; then
            printf 'Stopping Wiesel (PID %s)...\n' "$pid"
            kill -TERM "$pid" || { fail "Could not stop PID $pid; bundle unchanged. Retry after quitting Wiesel."; return 1; }
        fi
    done
    for ((attempt=0; attempt<50; attempt++)); do
        current="$(app_pids)"
        [[ -n "$current" ]] || return 0
        sleep 0.1
    done
    fail 'Wiesel did not exit within 5 seconds; bundle unchanged. Quit it manually and retry. No force-kill was sent.'
}

install_bundle() {
    # Both paths are on the same filesystem; retain the old bundle until installed.
    backup="$stage/previous.app"
    if [[ -e "$app" ]]; then mv "$app" "$backup"; fi
    if ! mv "$stage/Wiesel.app" "$app"; then
        fail 'Could not install new bundle; restoring the previous bundle.'
        return 1
    fi
    installed=1
}

run_build() {
    prepare "$@"
    stage_bundle
    local pids
    pids="$(app_pids)"
    if [[ -n "$pids" ]]; then
        fail 'This bundle is running. Use scripts/dev.sh to stop and restart it safely, or quit it before building.'
        return 1
    fi
    install_bundle
    printf '\nBuilt %s\nLaunch with: open "%s"\n' "$app" "$app"
}

run_dev() {
    prepare "$@"
    stage_bundle
    stop_app
    install_bundle
    open -n "$app" || { fail "Bundle installed, but launch failed. Retry: open \"$app\""; return 1; }
    printf '\nRestarted %s\n' "$app"
}
