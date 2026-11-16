#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Every hover help in SPM is one of two: the toolkit's Tooltip (buttons'
# .tooltip("…"), Tooltip::new) or the (i)'s InfoTip in app/src/ui.rs, which
# takes the same theme tokens. Fails when a tooltip view is built anywhere
# else, or when InfoTip's render uses a literal colour.
#   tests/layout/tooltips.sh
set -u
cd "$(dirname "$0")/../../app/src" || exit 2
bad=0
# A tooltip closure must build Tooltip::new or InfoTip.
while IFS= read -r hit; do
  file=${hit%%:*}; rest=${hit#*:}; line=${rest%%:*}
  body=$(sed -n "${line},$((line + 3))p" "$file")
  if ! grep -qE 'Tooltip::new|InfoTip' <<<"$body"; then
    echo "tooltip built outside the shared ones: $file:$line"; bad=1
  fi
done < <(grep -rnE '\.tooltip\(move \||\.tooltip\(\|' --include='*.rs' .)
# InfoTip's render: theme tokens only.
block=$(awk '/^impl Render for InfoTip/{on=1} on{print} on&&/^}/{exit}' ui.rs)
if grep -nE 'rgb\(0x|rgba\(0x|hsla\([0-9.]+, *[0-9.]+' <<<"$block" | grep -v 'black()' ; then
  echo "InfoTip uses a literal colour (use the theme's popover tokens)"; bad=1
fi
[ $bad -eq 0 ] && echo "tooltips.sh: every tooltip is the shared one"
exit $bad
