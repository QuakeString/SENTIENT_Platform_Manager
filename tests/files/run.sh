#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# The Files click test on both panes (tests/files/clicks.py): with reading
# ahead in light, without it in dark (every first visit shows the skeleton
# rows), against a real server; the folders it needs made first and
# removed after (tests/files/fixture.sh).
#
#   tests/files/run.sh <debug binary> <out dir> <XDG_CONFIG_HOME with the
#       instance> <instance id> <ssh arguments to that server…>
#
#   e.g. tests/files/run.sh target/debug/sentient-platform-manager /tmp/files \
#        ~/spm-test-profile ft -p 2242 -i ~/key ops@127.0.0.1
#
# FAR_INSTANCE: also the remote pane over that instance (the same server
# behind a slow link, e.g. a relay that adds 40 ms round trips).
# OPEN_MS (default 300) and BAR_MS (50): the limits asserted.
# Needs Xvfb, xdotool and ImageMagick's import. Exit status 1 on a failure.
set -u
BIN=${1:?binary}; OUT=${2:?out dir}; PROFILE=${3:?profile}; INST=${4:?instance}; shift 4
[ $# -gt 0 ] || { echo "run.sh: ssh arguments to the server" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
RHOME=$(ssh -o BatchMode=yes -o LogLevel=ERROR "$@" 'echo $HOME')
[ -n "$RHOME" ] || { echo "run.sh: the server does not answer" >&2; exit 2; }
RBASE="$RHOME/spm-files-test"; LBASE="$OUT/local/spm-files-test"
"$HERE/fixture.sh" make "$LBASE" && "$HERE/fixture.sh" make "$RBASE" "$@" || { echo "run.sh: no fixture" >&2; exit 2; }
rc=0
one() { # out theme instance sides [flags]
  python3 "$HERE/clicks.py" "$BIN" "$OUT/$1" --profile "$PROFILE" --instance "$3" --remote-base "$RBASE" --local-base "$LBASE" \
    --remote-home "$RHOME" --theme "$2" --sides "$4" --open-ms "${OPEN_MS:-300}" --bar-ms "${BAR_MS:-50}" "${@:5}" || rc=1
}
one ahead light "$INST" 0,1
one no-ahead dark "$INST" 0,1 --no-ahead
[ -n "${FAR_INSTANCE:-}" ] && one far light "$FAR_INSTANCE" 1 --no-ahead
"$HERE/fixture.sh" remove "$LBASE"; "$HERE/fixture.sh" remove "$RBASE" "$@"
echo "files: $([ $rc = 0 ] && echo ok || echo FAIL) (renders and logs in $OUT)"
exit $rc
