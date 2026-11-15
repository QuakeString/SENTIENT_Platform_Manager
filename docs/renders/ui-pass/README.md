# UI pass renders

Real renders of the Platform Manager (debug build, Xvfb, software Vulkan) for review.

- `after/page-<page>-<W>x<H>-<theme>.png`: every manager page at 1400x900, 1000x700, 760x600 and 680x480 (the minimum window), light and dark. Sample data: a fixture installation record and one remote instance with a canned status.
- `after/setup-<step>-980x680-<theme>.png`: every Setup step (0 Welcome … 9 Summary, 11 Finish), light and dark.
- `before/`: master before this pass (1effb29) at 1000x700, the main pages and two Setup steps, for side by side comparison with `after/`.
- `mockup/mockup-<tile>-<theme>.png`: the approved mockup's frames (spm-update-flow, version 38/39): g1/g2 Status, k1 Instances, k2 Add instance, k6 About, j2/j5 Backup, h1/h2 Settings, a1/b7 Update, c4 Setup.
