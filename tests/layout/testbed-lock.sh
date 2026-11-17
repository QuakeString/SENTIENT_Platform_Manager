#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Run a command holding the test-bed lock, so that it can never be held by
# a run that is gone:
#   - `flock -o`: the lock belongs to flock alone, never to the command's
#     children (Xvfb and the app inherited it before and kept it for
#     80 minutes after their run was stopped);
#   - the holder's pid, start time and command go into a sidecar file; a
#     waiter finds a dead holder, or processes still holding the lock whose
#     run is gone (parent 1), and frees it, saying so on stderr;
#   - a hard time limit (TIMEOUT seconds, default 5400): the command is
#     stopped with its whole process group and the run fails loudly;
#   - stopping this script stops the command's process group too.
#
#   tests/layout/testbed-lock.sh <command> [args…]
#   LOCK=/path/to/lock TIMEOUT=5400 WAIT=1800 tests/layout/testbed-lock.sh …
set -u
LOCK=${LOCK:-/mnt/data_1/INVENIA/spm-testbed.lock}
INFO="$LOCK.holder"
TIMEOUT=${TIMEOUT:-5400}
WAIT=${WAIT:-1800}
[ $# -gt 0 ] || { echo "usage: $0 <command> [args…]" >&2; exit 2; }

holders() { # pids with the lock file open, other than us
  for p in /proc/[0-9]*; do
    ls -l "$p/fd" 2>/dev/null | grep -q " -> $LOCK\$" && basename "$p"
  done | grep -vx "$$"
}

free_stale() {
  # A recorded holder that is gone, or orphans (parent 1) holding the lock:
  # their run ended without letting go.
  local pid=""
  [ -s "$INFO" ] && pid=$(awk '{print $1; exit}' "$INFO")
  for h in $(holders); do
    ppid=$(awk '/^PPid:/{print $2}' "/proc/$h/status" 2>/dev/null)
    case "$(tr '\0' ' ' < "/proc/$h/cmdline" 2>/dev/null)" in flock*) continue ;; esac
    if [ "$ppid" = 1 ] || { [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; }; then
      echo "testbed-lock: freeing stale holder $h ($(tr '\0' ' ' < /proc/$h/cmdline | cut -c1-80)); recorded holder ${pid:-none}" >&2
      kill "$h" 2>/dev/null
    fi
  done
}

free_stale
# The command in its own process group; flock -o keeps the lock out of it.
# shellcheck disable=SC2016
setsid flock -o -w "$WAIT" "$LOCK" bash -c '
  echo "$$ $(date -u +%FT%TZ) $*" > "'"$INFO"'"
  exec timeout --kill-after=30 '"$TIMEOUT"' "$@"
' bash "$@" &
child=$!
trap 'kill -TERM -- -"$child" 2>/dev/null; sleep 2; kill -KILL -- -"$child" 2>/dev/null' EXIT INT TERM
wait "$child"; rc=$?
trap - EXIT INT TERM
[ $rc -eq 124 ] && echo "testbed-lock: '$*' passed its time limit (${TIMEOUT}s) and was stopped" >&2
[ $rc -eq 1 ] && ! [ -s "$INFO" ] && echo "testbed-lock: could not take the lock within ${WAIT}s" >&2
: > "$INFO"
exit $rc
