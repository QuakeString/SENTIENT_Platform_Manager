#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# spm-watchdog: every heartbeat file in ~/.spm-heartbeats is
# "<count> <epoch> <pid> [total]", rewritten by a long job while it runs
# (tests/layout/run.sh writes one). A count that has not moved for 10
# minutes: log the job, its process tree and the test-bed lock's holders to
# ~/spm-watchdog.log, free orphaned lock holders (parent 1, not flock), and
# mark it STALLED (printed once on stdout). A heartbeat whose pid is gone is
# removed. Run it every 2 minutes (a systemd user timer, or a loop).
LOG=$HOME/spm-watchdog.log; DIR=$HOME/.spm-heartbeats; LOCK=${LOCK:-/mnt/data_1/INVENIA/spm-testbed.lock}
now=$(date +%s)
mkdir -p "$DIR"
for hb in "$DIR"/*; do
  [ -f "$hb" ] || continue
  case "$hb" in *.state) continue ;; esac
  read -r count _ pid total < "$hb" 2>/dev/null || continue
  st="$hb.state"
  if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then rm -f "$hb" "$st"; continue; fi
  read -r lastc lastt flag < "$st" 2>/dev/null || { lastc=-1; lastt=$now; flag=""; }
  if [ "$count" != "$lastc" ]; then echo "$count $now" > "$st"; continue; fi
  age=$(( now - lastt ))
  if [ $age -ge 600 ] && [ "$flag" != STALLED ]; then
    {
      echo "$(date '+%F %T') STALLED $(basename "$hb"): count $count/${total:-?} unchanged ${age}s, pid $pid"
      ps -o pid,ppid,etime,args --forest -g "$(ps -o sid= -p "$pid" 2>/dev/null | tr -d ' ')" 2>/dev/null | cut -c1-160 | head -30
      echo "  lock holder record: $(cat "$LOCK.holder" 2>/dev/null)"
      for p in /proc/[0-9]*; do
        ls -l "$p/fd" 2>/dev/null | grep -q " -> $LOCK\$" || continue
        h=$(basename "$p"); pp=$(awk '/^PPid:/{print $2}' "$p/status")
        echo "  lock held by $h (ppid $pp): $(tr '\0' ' ' < "$p/cmdline" | cut -c1-100)"
        case "$(tr '\0' ' ' < "$p/cmdline")" in flock*) ;; *) [ "$pp" = 1 ] && kill "$h" 2>/dev/null && echo "  freed orphan $h" ;; esac
      done
    } >> "$LOG"
    echo "$count $lastt STALLED" > "$st"
    echo "STALLED $(basename "$hb") count $count unchanged ${age}s (see ~/spm-watchdog.log)"
  fi
done
