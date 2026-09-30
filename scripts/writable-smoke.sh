#!/bin/sh
# Explicit invocation only; exit 77 is an unmet prerequisite, never a pass.
set -eu
exec timeout --signal=TERM --kill-after=10s 300s python3 "$(dirname "$0")/writable-smoke.py" "$@"
