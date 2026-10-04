#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/app-common.sh"
mode=dev
run_dev "$@"
