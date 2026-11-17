# SPM performance audit (rc12-24)

Branch `spm/rc12-24-perf` (from 73ebd3c). Measured 2026-11-17 on the
development machine (28 threads; other agents building at the same time).

## How it was measured

- **Builds:** debug only (no release-mode timing was made). Debug numbers
  overstate CPU and render times; the ratios between before and after are
  what to read.
- **Window:** Xvfb, 1100x720, scale 1. Xvfb has no GPU, so every frame is
  drawn by llvmpipe (software): idle CPU here is mostly the cost of the
  frames drawn, and it is much higher than on a real desktop.
- **Probes:** `SPM_DEBUG_TIMING=1` (crate::timing) prints `TIMING` marks: the
  start by stage, each page's view build the first frame it shows, and how
  often the window rendered in each 10 s. `SPM_DEBUG_GO_PAGES=page:secs,...`
  goes to pages in turn. `tests/layout/perf.sh` runs one page and reads
  `/proc/<pid>/stat` and `/proc/<pid>/status`. With `DOCKER_SHIM=1`, a
  `docker` that only logs its arguments comes first on PATH, so no container
  on this computer was touched.
- **Installation:** a Docker installation record whose stack does not exist
  (the shim answers nothing).
- **Before:** commit 7670db6 (the probes only). **After:** 35cdb5e.
- **Gate:** `tests/layout/perf_gate.sh <bin> <out>`, plus the unit tests
  `backups::tests::a_folder_listing_lists_the_disks_once` and
  `manager::perf_tests::a_meter_moves_by_whole_pixels`.

## Before / after

| Measure | Before | After |
|---|---|---|
| Start, theme step ("follow system"; this desktop's portal never answers) | 2014 ms (3 runs, ±3) | 310 ms (3 runs; 963 ms once on a loaded machine) |
| Launch to first frame, theme followed, warm | 2595 / 2625 ms | 808 / 1005 ms |
| Launch to first frame, theme forced (SPM_THEME=light), warm | 490–693 ms | 535–633 ms (no change expected) |
| Launch to first frame, cold (binary evicted with fadvise) | 1178–1439 ms (one run did not start within 5 s: the 1.1 GB debug binary loading) | 400–877 ms |
| docker calls in 30 s on About, after Status was opened | 11 (8 `ps -a`, 9 `ps -aq` …) | 0 |
| Backup page, view build on the UI thread (switching to it) | 190 / 437 / 290 ms | 10 / 4 / 9 ms |
| Idle window (About, animations on): renders per s in a 10 s window | 2.3–3.9 | 0.25–1.75 (follows the machine's load: CPU readings that move a bar by ≥ 1 px still glide) |
| Idle window, CPU (Xvfb/llvmpipe) | About 24.5 %, Status 28.6 % | About 10.9 % (one 15 s run) |
| Tray only, window closed (`--background`), CPU in 60 s | 330 ms | 180 ms |
| `dir_list` (folder chooser) disk enumerations, 40 subfolders | 42 (one per folder, plus 2) | 1 |

View build when switching to a page (ms, 3 runs; before ≈ after for every
page the fixes do not touch): Statistics 0.1–0.5, Files 0.1–0.5, Gateway
0.2–0.7, Settings 2–10, Logs 0.2, Status 2.5–3. These times cover only
creating the page's view; its first render is not included (see "Not
measured").

Memory, after, 30 minutes (Status, Logs with 500 lines at 15 min, then back
to Status): 249.7 MB at 5 min, 251.5 MB at 15 min, 282.3 MB after Logs (the
Logs view and its lines stay alive), 283.1 MB at 30 min. That is about
0.1 MB per minute, with no steady growth. Threads: 155 the whole time.

## Fixed (commits)

1. **718482b — theme at start.** `theme::apply(System)` ran `busctl` against
   the desktop portal on the UI thread before the window opened. Where the
   portal does not answer, every start waited busctl's 2 s timeout. The
   3-second follow loop also started a 2 s busctl every time, including
   with the window closed.
   - Now: the start waits 300 ms at most, then uses GNOME's setting (the
     same fallback as after a timeout before).
   - A portal that timed out is not asked again for 5 minutes.
   - With no window open, the follow loop does nothing.
2. **2b93950 — Backup page.** On a Docker install, `BackupPage::new` read
   the compose file through `bash -lc cat` and, failing that, two
   `docker inspect` calls, all on the UI thread (190–650 ms).
   - Now these are read in the background and fill the password fields
     that are still empty.
   - A "Read the database" asked for in the meantime runs once the
     passwords are in.
   - `dir_list` now lists the disks once per listing instead of once per
     subfolder. On this machine one listing (81 mounts) takes 0.7 ms, so the
     gain is small here. It is larger with many folders or slow network
     mounts, and it also runs on the agent.
3. **35cdb5e — hidden pages and meters.**
   - Status (`docker ps` through a login shell, plus the gateway probe:
     `docker ps -aq`/`inspect`/`exec`, or systemctl/ps, every 5 s), Gateway
     (5 s), Repair (10 s) and this computer's Logs (3 s × 500 lines) kept
     polling for the whole run once opened. They also kept polling while a
     server was open. Now they wait until their page shows again, checked
     every 400 ms.
   - This computer's meters are not read while a server is open.
   - The top bar re-renders only for a changed reading. Its bars glide only
     for a move of a whole pixel; before, each 4 s reading drew 400 ms of
     frames for moves smaller than a pixel.

## Ranked findings not fixed here (largest measured cost first)

1. **Every docker command goes through a login shell** (`bash -lc`,
   sentient-installer-core `distro::shell_args`).
   - Cost: on this machine `bash -lc true` costs 60 ms wall and about 80 ms
     CPU, against about 0 for `bash -c`. Under strace, one call from the app
     started about 45 processes (profile.d: im-config, debuginfod, locale)
     and took 730 ms.
   - Status polls `docker ps` this way every 5 s while it shows, which is
     about 1.6 % of a core for the shell alone. Install, update and backup
     jobs pay it on every step.
   - Proposal: capture the login environment once (`bash -lc 'env -0'`),
     then run `bash -c` (or docker directly) with it. Re-capture it when a
     command exits 127.
   - Not done here: the code is shared with the install and update jobs,
     and snap or rootless-docker PATH and DOCKER_HOST cases need testing.
   - Expected gain: 60–700 ms per docker call, and most of Status's CPU.
2. **Remote: one ssh process per call, with no reuse.** Each server page
   starts 2–4 fetches in parallel (`ready`, `status`, plus `services` and
   `dbstatus` for Status). Each fetch is its own ssh connection and its own
   agent start.
   - Through the 82 ms delaying proxy, that is about 4 round trips of
     handshake per call before any work is done (not measured, see below).
   - The spm/rc12-24-conn worker's persistent connection addresses this.
   - The tray checker (`bg_app::check_instances`) also asks every instance
     *sequentially* every 1–5 minutes. With N servers and one that does not
     answer, the round waits N × timeout. Parallelising it is a small change
     once the new layer is in.
3. **The tray refresh reads the local snapshot every 15 s** (`docker ps`
   through a login shell, see #1), even while the Status page reads the same
   snapshot every 5 s.
   - Sharing one reading (a global with its time) would halve the docker
     calls while the window is open.
   - The tray-only CPU measured here (180 ms per minute) had no tray, because
     Xvfb has no StatusNotifier host, so that part was not measured.
4. **Logs page: up to 500 rows re-built on every follow read (3 s).** The
   list is not virtualized; only the job view uses `uniform_list`.
   - Measured: the idle CPU of the Logs page with 500 lines was 25–27 %,
     the same as About (24.5 %) within noise, and the window rendered 0.6
     times per second.
   - It is not a cost at TAIL=500, but it would be with a larger tail.
   - Logs added about 31 MB of RSS that stays after leaving the page.
   - Expected gain is small unless the tail grows. Virtualize it when the
     tail is raised.
5. **The whole window renders for any notify.** The Manager is one view, so
   a meter reading re-renders the page too.
   - After the fixes, that happens about every 4 s when a reading changed.
   - Splitting the top bar into its own entity would make those renders
     cost only the bar. Expected gain: small on a real GPU.
6. **`reduce_motion()` is spelled `SPM_REDUCE_MOTION`, but
   tests/layout/run.sh sets `SPM_REDUCED_MOTION`.** The layout renders
   therefore run with animations on. They take their screenshots after 18 s
   so the results hold, but the variable does not do what the script means.
7. **Start with the tray enabled** registers the tray, autostart and the
   listen socket before the window opens. Measured at 4–15 ms: negligible.
   Opening the window (GPU init) took about 180 ms, and `Manager::new`
   140–340 ms in debug, not broken down further.

## Not measured

- **Remote pages through the delaying proxy (tb3).** The test-bed lock was
  held by other workers for the whole window (40+ minutes waiting), and the
  coordinator then reserved it for the 0.9.0 gate. `remote.sh` is in
  /mnt/data_1/INVENIA/spm-wip/perf/ and is ready to run against the new
  connection layer.
- **Agent-side cost per request** (docker, ps and du calls on the server):
  this needs the test bed too.
- **Release-mode timings** (start, idle CPU on a real GPU).
- **Each page's own first render** (Statistics, Files and the others render
  in their own views, outside the Manager's render timing).
- **Idle CPU on a real desktop with a GPU.** Xvfb draws frames on the CPU,
  so the absolute CPU numbers above apply only to Xvfb.
- **SVG and image rasterizing per frame:** not looked at.
