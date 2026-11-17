#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# The menu selection (shell.rs `selection`) is drawn with rounded quads
# only: GPUI antialiases quads exactly on every GPU, while paths get the
# driver's MSAA (on some Linux GPUs none, so the curves stair-step: his
# rc12-24 screenshot). Xvfb's software renderer smooths paths anyway, so a
# pixel test here cannot see the difference; this check holds the code to
# quads instead. The concave curves are inset shadows (the same distance-
# field antialiasing as a quad's corner), never shapes clipped by a content
# mask: GPUI's masks are hard-edged and snapped outward to device pixels
# (his 0.9.0 note: jagged curves). Fails when the selection painter uses a
# path or a content mask.
#   tests/layout/corners.sh
set -u
f="$(dirname "$0")/../../app/src/manager/shell.rs"
body=$(awk '/^pub\(crate\) fn selection\(/{on=1} on{print} on&&/^}/{exit}' "$f")
[ -n "$body" ] || { echo "corners.sh: selection() not found"; exit 1; }
if grep -nE 'PathBuilder|paint_path' <<<"$body"; then
  echo "corners.sh: the menu selection draws with a path (use rounded quads)"; exit 1
fi
if grep -nE 'with_content_mask|ContentMask' <<<"$body"; then
  echo "corners.sh: the menu selection clips with a content mask (hard-edged; use quads and inset shadows)"; exit 1
fi
echo "corners.sh: the menu selection is quads and inset shadows only"
