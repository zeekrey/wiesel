#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/app-common.sh"
mode=build-app
run_build "$@"
