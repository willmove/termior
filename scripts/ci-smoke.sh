#!/usr/bin/env bash
# Run one launch smoke of the desktop binary and check for its success marker.
#
# usage: scripts/ci-smoke.sh <ENV_VAR> <MARKER> <LOG_NAME> -- <command...>
#
# Sets ENV_VAR=1 for <command>, guards it with a 120 s watchdog, writes combined output to
# $SMOKE_LOG_DIR/<LOG_NAME>.log (default: smoke-logs/) and fails unless the command exits 0 and
# prints MARKER. The log directory is uploaded as a CI artifact when a smoke fails.
set -uo pipefail

if [ "$#" -lt 5 ] || [ "$4" != "--" ]; then
  echo "usage: $0 <ENV_VAR> <MARKER> <LOG_NAME> -- <command...>" >&2
  exit 2
fi
var=$1 marker=$2 name=$3
shift 4

logdir=${SMOKE_LOG_DIR:-smoke-logs}
mkdir -p "$logdir"
log="$logdir/$name.log"

if command -v timeout >/dev/null 2>&1; then
  guard=(timeout 120)
else
  # macOS runners have no coreutils `timeout`.
  guard=(perl -e 'alarm 120; exec @ARGV' --)
fi

env "$var=1" "${guard[@]}" "$@" >"$log" 2>&1
status=$?
cat "$log"

if [ "$status" -ne 0 ]; then
  echo "::error::$name exited with status $status" >&2
  exit 1
fi
if ! grep -q "$marker" "$log"; then
  echo "::error::$name did not print $marker" >&2
  exit 1
fi
