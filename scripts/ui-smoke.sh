#!/usr/bin/env bash
# Compile a stable-path native runner. No bundle rebuild/restart or permission changes.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
if [[ "$(uname -s)" != Darwin ]]; then
    printf 'Native UI smoke tests require macOS.\n' >&2
    exit 2
fi
source_file="$root/scripts/ui-smoke.swift"
runner_dir="$root/target/ui-smoke"
runner="$runner_dir/wiesel-ui-smoke"
mkdir -p "$runner_dir"
if [[ ! -x "$runner" || "$source_file" -nt "$runner" ]]; then
    staged="$(mktemp "$runner_dir/.runner.XXXXXX")"
    trap 'rm -f "$staged"' EXIT
    xcrun swiftc -swift-version 5 -O -framework Cocoa -framework ApplicationServices \
        "$source_file" -o "$staged"
    chmod 755 "$staged"
    mv -f "$staged" "$runner"
fi
cd "$root"
exec "$runner" "$@"
