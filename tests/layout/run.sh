#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Layout test: every page at the minimum window width and at 900 and 1200
# px, scale 1 and 1.5, light and dark, this computer and (with INSTANCE set
# to an instance id in the profile) a server; check.py fails a render where
# anything reaches into a card's right padding or the page's right margin.
#
#   tests/layout/run.sh <app binary> <out dir> [XDG_CONFIG_HOME with instances]
#
# fields.py checks the settings card's fields line up and its Apply bar sits
# at the card's bottom (PENDING sets the states shown with the bar); rows.py
# that every input, segmented choice and control side by side in a row is
# the one standard height on one centre line; buttons.py that every button
# is the one button size; inputs.py the same in the source.
# MOVE_STEPS (default: every step of "Move data to another disk") and
# MOVE_WIDTHS add that wizard's renders; empty MOVE_STEPS leaves them out.
#
# Needs Xvfb and ImageMagick's import. Exit status 1 on any finding.
set -u
BIN=${1:?binary}; OUT=${2:?out dir}; PROFILE=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
PAGES_LOCAL=${PAGES_LOCAL-"status statistics logs files update gateway backup migration repair licence settings instances about"}
PAGES_REMOTE=${PAGES_REMOTE-"status statistics logs files update gateway backup migration repair licence settings"}
WIDTHS=${WIDTHS:-"560 900 1200"}; SCALES=${SCALES:-"1 1.5"}; THEMES=${THEMES:-"light dark"}
JOBS=${JOBS:-3}
# The record fixtures (fixtures/record-*.json) as the renders read them: a
# copy in $OUT/records with @MANAGER_VERSION@ filled from app/Cargo.toml,
# so a fixture that means "this Platform Manager" never goes stale.
MANAGER_VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$HERE/../../app/Cargo.toml" | head -1)
[ -n "$MANAGER_VERSION" ] || { echo "run.sh: no version in app/Cargo.toml" >&2; exit 2; }
RECORDS="$OUT/records"; rm -rf "$RECORDS"; mkdir -p "$RECORDS"
for f in "$HERE"/fixtures/record-*.json; do sed "s/@MANAGER_VERSION@/$MANAGER_VERSION/g" "$f" > "$RECORDS/$(basename "$f")"; done
one() { # where page width scale theme [dialog env]
  local where=$1 page=$2 w=$3 sc=$4 th=$5 dlg=${6:-} h=760
  # The dialog's env in the file name: "=" and "/" made safe.
  local tag="${dlg//=/-}"; tag="${tag//\//_}"
  local name="$where-$page-$w-x$sc-$th${dlg:+-$tag}" x="$OUT/xdg-$where-$page-$w-$sc-$th${dlg:+-$tag}"
  # SKIP_DONE="<out dir> …" (earlier runs, this one's own too): a render
  # that passed there is not done again (a stopped run finished later).
  local done_dir
  for done_dir in ${SKIP_DONE:-}; do
    if [ -e "$done_dir/$name.png" ] && [ -e "$done_dir/$name.result" ] && [ ! -s "$done_dir/$name.result" ]; then
      echo "ok   $name (passed in $done_dir)"; return 0
    fi
  done
  rm -rf "$x"; mkdir -p "$x"
  # "loading": a server page with its readings held back (LOADING_PROFILE
  # points its instance at an address that does not answer).
  local prof=$PROFILE wait=${WAIT:-18}
  [ "$where" = loading ] && { prof=$LOADING_PROFILE; wait=9; }
  # "stopped": no online update check (fixtures/profile-no-update-check:
  # daily checks off), so no "… is available" bar under the top bar that
  # depends on the network and on what is installed here (tint.py samples
  # the page just under the bar; the bar made it 8 findings, rc5 build 3).
  [ "$where" = stopped ] && prof=$HERE/fixtures/profile-no-update-check
  [ -n "$prof" ] && cp -r "$prof"/. "$x"/ && rm -f "$x"/sentient-platform-manager/running.lock
  local pw ph; pw=$(python3 -c "print(int($w*$sc))"); ph=$(python3 -c "print(int($h*$sc))")
  # A display of its own: Xvfb refuses one in use (its lock file), so try
  # numbers until one starts (parallel runs must never share a screen).
  local d xp n
  for n in $(seq 1 50); do
    d=":$((200 + RANDOM % 700))"
    Xvfb "$d" -screen 0 "${pw}x${ph}x24" -nolisten tcp >/dev/null 2>&1 & xp=$!
    sleep 1
    if kill -0 $xp 2>/dev/null && [ -e "/tmp/.X11-unix/X${d#:}" ]; then break; fi
  done
  local inst=(); [ "$where" != local ] && [ "$where" != setup ] && [ "$where" != maint ] && [ "$where" != stopped ] && inst=(--instance "${INSTANCE:?}")
  local mode=(--manager --page "$page"); [ "$where" = setup ] && mode=(--install)
  # "maint": Setup run again on an installation (its update tick boxes), on
  # the record fixture the page names (fixtures/record-<page>.json, its
  # filled copy in $RECORDS).
  local rec=${SPM_INSTALLATION_RECORD:-/nonexistent}
  [ "$where" = maint ] && { mode=(--maintenance); rec=$RECORDS/record-$page.json; }
  # "stopped": this computer's page on a Docker installation (the record
  # fixture) whose SENTIENT is stopped, its readings left out (on Windows
  # its WSL distro is never started for a read).
  [ "$where" = stopped ] && rec=$RECORDS/record-docker.json
  # A dialog's env: KEY=VAL, several joined with "+".
  local extra=(); [ -n "$dlg" ] && IFS=+ read -r -a extra <<<"$dlg"
  [ "$where" = stopped ] && { local more=(); IFS=+ read -r -a more <<<"$STOPPED_ENV"; extra+=("${more[@]}"); }
  env "${extra[@]}" GPUI_X11_SCALE_FACTOR=$sc DISPLAY=$d WAYLAND_DISPLAY= XDG_CONFIG_HOME=$x SPM_THEME=$th SPM_WINDOW_SIZE=${w}x${h} \
    SPM_INSTALLATION_RECORD=$rec SPM_DEBUG_LAYOUT=1 SPM_REDUCED_MOTION=1 \
    "$BIN" "${mode[@]}" "${inst[@]}" 2>"$OUT/$name.log" >/dev/null & local ap=$!
  sleep "$wait"
  # HOVER_INFO=1: the pointer on the page's first (i), its tooltip open.
  if [ -n "${HOVER_INFO:-}" ]; then
    local at; at=$(grep '^LAYOUT info-icon ' "$OUT/$name.log" | tail -1 | awk -v s="$sc" '{printf "%d %d", ($3+$4)/2*s, ($5+$6)/2*s}')
    [ -n "$at" ] && { DISPLAY=$d xdotool mousemove $at; sleep 2; }
  fi
  DISPLAY=$d import -window root "$OUT/$name.png" 2>/dev/null
  kill $ap 2>/dev/null; sleep 1; kill -9 $ap 2>/dev/null; kill $xp 2>/dev/null
  rm -rf "$x"
  # The page's overflow, and the menu's icons on x = 32 (folded below
  # 1000 px, unfolded above).
  local only=(); { [ "$where" = setup ] || [ "$where" = maint ]; } && only=(buttons)
  # A sub-flow's render: it is open, and the menu keeps its parent page.
  local expect=(); case "$dlg" in SPM_DEBUG_MOVE_STEP=*) expect=(EXPECT_SUB=movedata EXPECT_MENU="$page");; esac
  { env "${expect[@]}" python3 "$HERE/check.py" "$OUT/$name.png" "$OUT/$name.log" "$sc" "${only[@]}" && { [ "$where" = setup ] || [ "$where" = maint ] || python3 "$HERE/icons.py" "$OUT/$name.log"; } && python3 "$HERE/fields.py" "$OUT/$name.log" "$dlg" && python3 "$HERE/rows.py" "$OUT/$name.log" && python3 "$HERE/buttons.py" "$OUT/$name.log"; } > "$OUT/$name.result" 2>&1 && echo "ok   $name" || { echo "FAIL $name"; cat "$OUT/$name.result"; }
}
export -f one; export RECORDS OUT HERE BIN PROFILE LOADING_PROFILE INSTANCE WAIT SPM_INSTALLATION_RECORD HOVER_INFO SKIP_DONE 2>/dev/null
list=()
for p in $PAGES_LOCAL; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local $p $w $s $t"); done; done; done; done
# Dialogs and panels, opened by their debug hooks (this computer).
DIALOGS=${DIALOGS-"SPM_DEBUG_TAKEOVER=found SPM_DEBUG_LOG_DIALOG=1 SPM_DEBUG_PARTS_DIALOG=1 SPM_DEBUG_BELL=1 SPM_DEBUG_SUDO_PROMPT=1 SPM_DEBUG_ERROR_DIALOG=1 SPM_DEBUG_LICENCES_DIALOG=1"}
for g in $DIALOGS; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local status $w $s $t $g"); done; done; done; done
# The shared job view mid-way with a very long log (SPM_DEBUG_JOB_LOG,
# crate::manager::jobview), on the Backup page.
JOBVIEWS=${JOBVIEWS-"SPM_DEBUG_JOB_LOG=6000:2"}
for g in $JOBVIEWS; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local backup $w $s $t $g"); done; done; done; done
# The folder chooser over Backup: a long folder scrolled to its end (only
# the list scrolls: check.py holds the path bar at the dialog's top), and
# at the minimum width. CHOOSER_WIDTHS empty leaves them out.
CW=${CHOOSER_WIDTHS-560 1200}
for w in $CW; do for s in $SCALES; do for t in $THEMES; do list+=("local backup $w $s $t SPM_DEBUG_FOLDER_CHOOSER=1+SPM_DEBUG_FOLDER_PICK=/usr/lib/x86_64-linux-gnu+SPM_DEBUG_CHOOSER_SCROLL=bottom"); done; done; done
# Add instance with its Setup guide open (Linux and Windows servers).
for g in ${GUIDES-SPM_DEBUG_ADD_GUIDE=linux SPM_DEBUG_ADD_GUIDE=windows}; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local instances $w $s $t $g"); done; done; done; done
# "Move data to another disk" at each step, a sub-flow of Migration (the
# canned server; the menu must keep Migration selected), from the minimum
# width up through the narrow and rail layouts.
MOVE_WIDTHS=${MOVE_WIDTHS:-"560 760 960 1200"}
for g in ${MOVE_STEPS-SPM_DEBUG_MOVE_STEP=what SPM_DEBUG_MOVE_STEP=disk SPM_DEBUG_MOVE_STEP=folder SPM_DEBUG_MOVE_STEP=plan SPM_DEBUG_MOVE_STEP=run}; do for w in $MOVE_WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local migration $w $s $t $g"); done; done; done; done
# Statistics with the Server card (crate::manager::server_card) on canned
# readings: a physical Linux machine with a graphics card and three disks,
# one nearly full; a virtual machine; Windows. Each with All details open
# too, and the restart job mid-way in the shared job view.
SERVER_FIXTURES=${SERVER_FIXTURES-"linux-gpu vm windows"}
STATS_FIXTURE="$HERE/fixtures/stats"
for fx in $SERVER_FIXTURES; do
  for g in "" "+SPM_DEBUG_SERVER_DIALOG=details"; do
    for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local statistics $w $s $t SPM_DEBUG_STATS=$STATS_FIXTURE+SPM_DEBUG_SERVER_INFO=$fx$g"); done; done; done
  done
done
[ -n "$SERVER_FIXTURES" ] && for g in ${SERVER_JOBS-SPM_DEBUG_SERVER_DIALOG=shutdown SPM_DEBUG_SERVER_DIALOG=job:restart+SPM_DEBUG_SERVER_RESTART=waiting}; do
  for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local statistics $w $s $t SPM_DEBUG_STATS=$STATS_FIXTURE+SPM_DEBUG_SERVER_INFO=vm+$g"); done; done; done
done
# The Gateway page on canned gateways (SPM_DEBUG_GATEWAY, fixtures in
# crate::manager::gateway::debug_fixture), with a newer release offered:
# Docker, a compose container, a native package service, one whose
# settings are closed to this account, one by hand, the Windows service,
# and the older Python gateway beside none and beside ours.
GATEWAY_FIXTURES=${GATEWAY_FIXTURES-"docker compose native native-unreadable native-hand windows legacy docker-legacy"}
for g in $GATEWAY_FIXTURES; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local gateway $w $s $t SPM_DEBUG_GATEWAY=$g+SPM_DEBUG_GATEWAY_OFFER=4.16.3"); done; done; done; done
# The Update page's gateway line: ours in Docker, a package service (off,
# with its reason) and the older Python gateway (with its (i)).
for g in ${UPDATE_GATEWAY_FIXTURES-docker native legacy}; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local update $w $s $t SPM_DEBUG_GATEWAY=$g+SPM_DEBUG_GATEWAY_OFFER=4.16.3"); done; done; done; done
# Setup's update tick boxes with the gateway's line: beside a newer Platform
# Manager (record-docker), and alone (record-docker-same: this version,
# @MANAGER_VERSION@ filled in).
for r in ${MAINT_RECORDS-docker docker-same}; do for g in ${MAINT_GATEWAY_FIXTURES-docker legacy}; do for w in ${MAINT_WIDTHS-900 1200}; do for s in $SCALES; do for t in $THEMES; do list+=("maint $r $w $s $t SPM_DEBUG_GATEWAY=$g+SPM_DEBUG_GATEWAY_OFFER=4.16.3+SPM_DEBUG_MANAGER_INSTALLED=1+SPM_DEBUG_BROKEN=0"); done; done; done; done; done
if [ -n "${INSTANCE:-}" ]; then
  for g in ${REMOTE_UPDATE_GATEWAY_FIXTURES-docker legacy}; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("remote update $w $s $t SPM_DEBUG_REMOTE_GATEWAY=$g+SPM_DEBUG_GATEWAY_OFFER=4.16.3"); done; done; done; done
  for g in ${REMOTE_GATEWAY_FIXTURES-native native-unreadable docker-legacy}; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("remote gateway $w $s $t SPM_DEBUG_REMOTE_GATEWAY=$g+SPM_DEBUG_GATEWAY_OFFER=4.16.3"); done; done; done; done
fi
# SENTIENT stopped (Windows Docker with its distro stopped, any Docker
# install with the stack down): Status's Database card, Statistics'
# Services, Backup and Platform settings' administrators say so plainly
# (canned readings: SPM_DEBUG_DBSTATUS, SPM_DEBUG_STATS,
# SPM_DEBUG_SENTIENT_ADMIN=stopped).
# The stack's containers stopped and SENTIENT not answering
# (SPM_DEBUG_SNAPSHOT), whatever a run exports for its running renders.
export STOPPED_ENV="SPM_DEBUG_DBSTATUS=$HERE/fixtures/dbstatus-stopped.json+SPM_DEBUG_STATS=$HERE/fixtures/stats-stopped+SPM_DEBUG_SERVER_INFO=vm+SPM_DEBUG_SENTIENT_ADMIN=stopped+SPM_DEBUG_SNAPSHOT=false;sentient=stopped,sentient-postgres=stopped"
for p in ${STOPPED_PAGES-status statistics backup settings}; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("stopped $p $w $s $t"); done; done; done; done
# The Gateway page's settings card (crate::manager::gateway_settings) on
# the settings fixture of each canned gateway, every tab
# (SPM_DEBUG_GATEWAY_TAB), here and on the server; then the Apply bar with
# changes pending and the Apply job view (SPM_DEBUG_GATEWAY_PENDING).
# GW_LOCAL=0 leaves this computer's out (a run of the server's only).
GW_SETTINGS_FIXTURES=${GW_SETTINGS_FIXTURES-}
GW_TABS=${GW_TABS-"0 1 2 3 4 5 6"}
GW_PENDING=${GW_PENDING-"SPM_DEBUG_GATEWAY_TAB=6+SPM_DEBUG_GATEWAY_PENDING=1 SPM_DEBUG_GATEWAY_TAB=1+SPM_DEBUG_GATEWAY_PENDING=1 SPM_DEBUG_GATEWAY_PENDING=apply"}
for g in $GW_SETTINGS_FIXTURES; do
  for e in $(for t in $GW_TABS; do echo "SPM_DEBUG_GATEWAY_TAB=$t"; done) $GW_PENDING; do
    for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do
      [ "${GW_LOCAL:-1}" = 1 ] && list+=("local gateway $w $s $t SPM_DEBUG_GATEWAY=$g+$e")
      [ -n "${INSTANCE:-}" ] && list+=("remote gateway $w $s $t SPM_DEBUG_REMOTE_GATEWAY=$g+$e")
    done; done; done
  done
done
# Setup's window (its first step): the buttons' gaps only.
[ "${SETUP:-1}" = 1 ] && for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("setup setup $w $s $t"); done; done; done
if [ -n "${LOADING_PROFILE:-}" ]; then
  LW=${LOADING_WIDTHS:-560}; LS=${LOADING_SCALES:-1 1.5 2}
  for p in $PAGES_REMOTE; do for w in $LW; do for s in $LS; do for t in $THEMES; do list+=("loading $p $w $s $t"); done; done; done; done
fi
if [ -n "${INSTANCE:-}" ]; then
  for p in $PAGES_REMOTE; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("remote $p $w $s $t"); done; done; done; done
  # Platform settings with changes not applied (the Apply bar): valid
  # changes, a field not valid on a short and on a long tab, the leave
  # prompt (SPM_DEBUG_SETTINGS_PENDING).
  for g in ${PENDING-SPM_DEBUG_SETTINGS_PENDING=valid SPM_DEBUG_SETTINGS_PENDING=short SPM_DEBUG_SETTINGS_PENDING=long SPM_DEBUG_SETTINGS_PENDING=leave}; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("remote settings $w $s $t $g"); done; done; done; done
fi
# A heartbeat for the watchdog (spm-watchdog): the count of finished
# renders and when it last moved, in $HEARTBEAT (default under ~/.spm-heartbeats).
HB=${HEARTBEAT:-$HOME/.spm-heartbeats/layout-$(basename "$OUT")-$$}
mkdir -p "$(dirname "$HB")"
( while kill -0 $$ 2>/dev/null; do echo "$(ls "$OUT"/*.result 2>/dev/null | wc -l) $(date +%s) $$ ${#list[@]}" > "$HB"; sleep 30; done; rm -f "$HB" ) &
# Stopped or killed: take every render (Xvfb and the app) down with it.
trap 'pkill -TERM -P $$ 2>/dev/null; for x in $(pgrep -u "$(id -u)" -f "^Xvfb :[0-9]+ " ); do [ "$(awk "/^PPid:/{print \$2}" /proc/$x/status 2>/dev/null)" = 1 ] && kill $x 2>/dev/null; done' INT TERM
printf '%s\n' "${list[@]}" | xargs -P "$JOBS" -L 1 bash -c 'one "$@"' _ | tee "$OUT/summary.txt"
fails=$(grep -c '^FAIL' "$OUT/summary.txt"); total=$(grep -c -E '^(ok|FAIL)' "$OUT/summary.txt")
echo "layout test: $((total - fails))/$total passed"
# Across renders: a selected menu label the same size as unselected, one row
# pitch, the space under the brand row (labels.py).
python3 "$HERE/labels.py" "$OUT" | tee "$OUT/labels.txt"; labels=${PIPESTATUS[0]}
# The top bar's instance tint as faint in dark as in light (tint.py).
python3 "$HERE/tint.py" "$OUT" | tee "$OUT/tint.txt"; tint=${PIPESTATUS[0]}
# Every tooltip the shared one, with theme colours (tooltips.sh).
"$HERE/tooltips.sh" | tee "$OUT/tooltips.txt"; tips=${PIPESTATUS[0]}
# One control height in the source: inputs, buttons, segmented (inputs.py).
python3 "$HERE/inputs.py" | tee "$OUT/inputs.txt"; inputs=${PIPESTATUS[0]}
# The menu selection drawn with quads only (corners.sh).
"$HERE/corners.sh" | tee "$OUT/corners.txt"; corners=${PIPESTATUS[0]}
# Every long job in the shared job view, its steps declared (jobview.sh).
"$HERE/jobview.sh" | tee "$OUT/jobview.txt"; jobs=${PIPESTATUS[0]}
# One button size across every render (each scale, theme and page).
python3 "$HERE/buttons.py" "$OUT" | tail -5 | tee "$OUT/buttons.txt"; btns=${PIPESTATUS[0]}
[ "$fails" -eq 0 ] && [ "$labels" -eq 0 ] && [ "$tint" -eq 0 ] && [ "$tips" -eq 0 ] && [ "$corners" -eq 0 ] && [ "$jobs" -eq 0 ] && [ "$inputs" -eq 0 ] && [ "$btns" -eq 0 ]
