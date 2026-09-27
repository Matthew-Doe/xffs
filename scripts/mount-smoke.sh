#!/bin/sh
# Explicit invocation only; do not turn missing FUSE into a successful test.
set -eu
exec timeout --signal=TERM --kill-after=10s 240s python3 "$(dirname "$0")/mount-smoke.py"
