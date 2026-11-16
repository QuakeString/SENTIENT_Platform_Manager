#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Layout test for the SENTIENT Setup window: every wizard step (the debug
# hook SPM_WIZARD_STEP), at each width, scale and theme, with the short and
# the long variant of a step's content (SPM_DEBUG_SETUP_LONG=1). check.py
# fails a render where anything reaches into a card's right padding or the
# page's margin; icons.py checks the rail's badges sit on x = 32; labels.py
# checks the rail's rows (40 px on a 44 px pitch, 16 px under the brand) and
# that the current step's label is the size of the others.
#
#   tests/layout/setup.sh <app binary> <out dir>
#
# STEPS, WIDTHS (window width; the height is HEIGHT), SCALES, THEMES,
# VARIANTS ("short long") and JOBS narrow it. Needs Xvfb and ImageMagick.
set -u
BIN=${1:?binary}; OUT=${2:?out dir}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
STEPS=${STEPS:-"0 1 2 3 4 5 6 7 8 9 10 11"}
WIDTHS=${WIDTHS:-"1100"}; HEIGHT=${HEIGHT:-760}
SCALES=${SCALES:-"1 1.5 2"}; THEMES=${THEMES:-"light dark"}
VARIANTS=${VARIANTS:-"short long"}
JOBS=${JOBS:-2}
one() { # step width scale theme variant
  local step=$1 w=$2 sc=$3 th=$4 var=$5 h=$HEIGHT
  # labels.py groups renders by the name's -<width>-x<scale>-<theme> tail.
  local name="setup-$step-$var-$w-x$sc-$th" x="$OUT/xdg-$step-$var-$w-$sc-$th"
  rm -rf "$x"; mkdir -p "$x"
  local pw ph; pw=$(python3 -c "print(int($w*$sc))"); ph=$(python3 -c "print(int($h*$sc))")
  local d xp n
  for n in $(seq 1 50); do
    d=":$((200 + RANDOM % 700))"
    Xvfb "$d" -screen 0 "${pw}x${ph}x24" -nolisten tcp >/dev/null 2>&1 & xp=$!
    sleep 1
    if kill -0 $xp 2>/dev/null && [ -e "/tmp/.X11-unix/X${d#:}" ]; then break; fi
  done
  local long=(); [ "$var" = long ] && long=(SPM_DEBUG_SETUP_LONG=1)
  env "${long[@]}" GPUI_X11_SCALE_FACTOR=$sc DISPLAY=$d WAYLAND_DISPLAY= XDG_CONFIG_HOME=$x SPM_THEME=$th SPM_WINDOW_SIZE=${w}x${h} \
    SPM_INSTALLATION_RECORD=/nonexistent SPM_DEBUG_LAYOUT=1 SPM_REDUCED_MOTION=1 SPM_REDUCE_MOTION=1 SPM_WIZARD_STEP=$step \
    SPM_DEBUG_SETUP_OFFLINE=1 "$BIN" --install 2>"$OUT/$name.log" >/dev/null & local ap=$!
  sleep "${WAIT:-14}"
  DISPLAY=$d import -window root "$OUT/$name.png" 2>/dev/null
  kill $ap 2>/dev/null; sleep 1; kill -9 $ap 2>/dev/null; kill $xp 2>/dev/null
  rm -rf "$x"
  { python3 "$HERE/check.py" "$OUT/$name.png" "$OUT/$name.log" "$sc" && python3 "$HERE/icons.py" "$OUT/$name.log"; } > "$OUT/$name.result" 2>&1 && echo "ok   $name" || { echo "FAIL $name"; cat "$OUT/$name.result"; }
}
export -f one; export OUT HERE BIN HEIGHT WAIT
list=()
for s in $STEPS; do for v in $VARIANTS; do for w in $WIDTHS; do for c in $SCALES; do for t in $THEMES; do list+=("$s $w $c $t $v"); done; done; done; done; done
printf '%s\n' "${list[@]}" | xargs -P "$JOBS" -L 1 bash -c 'one "$@"' _ | tee "$OUT/summary.txt"
fails=$(grep -c '^FAIL' "$OUT/summary.txt"); total=$(grep -c -E '^(ok|FAIL)' "$OUT/summary.txt")
echo "setup layout test: $((total - fails))/$total passed"
python3 "$HERE/labels.py" "$OUT" | tee "$OUT/labels.txt"; labels=${PIPESTATUS[0]}
[ "$fails" -eq 0 ] && [ "$labels" -eq 0 ]
