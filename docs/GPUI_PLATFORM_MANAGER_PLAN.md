# SENTIENT Platform Manager on GPUI — plan

Status: approved 2026-10-15 ("go ahead build it"). Mockup of the installer flow:
https://claude.ai/artifact/XAuTL6QTMt1xu9ZycQ21k8 (private to the owner).

## What changes and why

1. **One installer, one step.** Today the operator installs the Platform
   Manager (SPM) with `installer/`, opens it, and runs its Setup wizard to
   install SENTIENT. From now on a single `SENTIENT-Setup.exe` installs SPM
   **and** SENTIENT (database, services, configuration) in one wizard. After
   that, SPM is the one app that manages the platform.
2. **GPUI instead of Tauri.** The UI moves from Tauri (WebView2 / WebKitGTK +
   HTML/JS) to GPUI with GPUI Kit components. Reasons: no WebView dependency
   (WebView2 is missing on some LTSC / air-gapped plant PCs), no WebKitGTK
   quirks, sharp at any DPI, one self-contained binary.
3. **No third-party installer technology.** No NSIS, Inno, MSI/WiX or Tauri
   bundler. The setup program is our own GPUI binary.
4. **Windows 10 and later only** for the installer. Linux keeps its current
   packages until decided otherwise.
5. **Licences stay in SENTIENT.** Activation is online-only and runs inside
   SENTIENT (`LicenseService::import_license_file`: "Offline licenses are not
   supported"), so neither the installer nor SPM manages licences. The Finish
   page only reminds the sys_admin to activate in SENTIENT.

## Install modes on Windows (user chooses in the wizard)

| Choice | SENTIENT | PostgreSQL | Time-series |
|---|---|---|---|
| PostgreSQL only | native service | native service | plain partitioned `ts_kv` |
| PostgreSQL + TimescaleDB | native service | native service | TimescaleDB extension |
| PostgreSQL + ClickHouse | native service | native service | ClickHouse in Docker inside WSL2 |

SENTIENT always runs natively: its licence binding uses the Windows hardware
fingerprint (MachineGuid, SMBIOS, disk serials, CPU, physical NICs), which is
stable across reboots, unlike a fingerprint taken inside WSL2/Docker. ClickHouse
is not licence-checked, so running it in WSL2 is fine. Nothing is preselected;
"PostgreSQL + ClickHouse" carries a *Recommended* tag. The choice is persisted so
update, backup and migration know what is installed.

## One binary, three entry modes

`SENTIENT-Setup.exe` and the installed `SENTIENT Platform Manager.exe` are the
same GPUI program:

- **Install mode** — not installed on this machine: the wizard.
- **Maintenance mode** — setup run again where SENTIENT is installed:
  Update · Repair · Change database (migration) · Uninstall.
- **Manager mode** — started from the Start menu: the management app.

`installer/` (the separate Tauri setup program) and `src-tauri/` are deleted at
the end of the port.

## Installer wizard (12 pages)

1. Welcome + licence agreement (Next disabled until accepted).
2. System check — Windows 10+, admin, disk, RAM, CPU, free ports, VC++ runtime
   ("Will be installed"), virtualization (only matters for ClickHouse).
3. Location — program, data, logs folders.
4. Database — the three choices above.
5. Database settings — PostgreSQL name/user/password/admin password/port;
   ClickHouse db/user/password/HTTP and native ports when chosen.
6. Network ports — HTTP, MQTT, MQTTS, CoAP, CoAPS, each checked free.
7. Administrator — first sign-in, optional demo data.
8. Configuration — *Recommended* (sized from cores/RAM) or *Customize*: every
   SENTIENT setting that applies to a single Windows node (see Settings catalog).
9. Shortcuts and startup.
10. Summary — every choice with *Change*.
11. Install — progress + task list; resumes after a WSL2 reboot; on failure
    Retry or Roll back.
12. Finish — address, sign-in, licence reminder, Open SENTIENT / Open SPM.

Pages 1–10 change nothing on the machine.

## Settings catalog

One data file (`settings_catalog`) describes every tunable SENTIENT
environment variable: label, variable, group, type, SENTIENT default,
installer recommendation, help text, whether a change needs a restart. It
drives the Customize page **and** SPM's Settings screen, and it is the single
place to update when SENTIENT adds a variable.

Groups: Server and web · Device transports · Database connections · PostgreSQL
server · Time-series storage · Write batching · Caches · Rule engine ·
Calculated fields · Device state · Retention and cleanup · Dashboards and
reports · Logging · Advanced (licence server URL, licence check interval,
instance ID, analytics) · Extra variables (free-form NAME=value).

Excluded: cluster-only (`NATS_URL`, `VALKEY_URL`, `MESSAGE_BUS_TYPE`,
`SENTIENT_CLUSTER_ENABLED`), build/test-only (`GIT_HASH`, `SENTIENT_TEST_*`,
`WEBKIT_*`, `WSL_*`, …). Secrets (`JWT_SECRET`, `VC_ENCRYPTION_KEY`,
`LICENSE_ENCRYPTION_KEY`) and the instance ID are generated, never typed.

## Platform Manager screens (after install)

Status · Control · Logs · Update · Backup / Restore · Migration · Settings ·
Repair / Uninstall. No licence features.

## Feature work after the port (A–D) — as built

Done 2026-10-16/17, each verified end to end (commits on `feature/gpui`).

- **A. Windows hybrid** — native SENTIENT + PostgreSQL, ClickHouse in Docker
  in SENTIENT's WSL2 distro, owned by the hidden local account
  `sentient-clickhouse` with an on-start task (runs with nobody signed in;
  survives a reboot). The account's WSL has no WSLg (its compositor crashed
  in a session without a display, ~120 MB dump each time). Secrets are
  machine-wide (DPAPI, `%ProgramData%\SENTIENT\secrets`).
- **B. Backup v2** — ClickHouse history per month (Native format, checksum
  per member) and latest values; restore into a new ClickHouse database
  beside the live one. Plain PostgreSQL's monthly `ts_kv` partitions are kept
  out of the dump (history travels once, in the telemetry member).
  "Put the restored data into service" then swaps it in (PostgreSQL rename,
  ClickHouse `EXCHANGE TABLES`), keeping the data from before and swapping
  back if SENTIENT does not start on it.
  *Not built:* restore across database types (a backup restores into the
  type it was taken from; migrate afterwards).
- **C. Migrations** — any direction between plain PostgreSQL, TimescaleDB and
  ClickHouse on Windows native and Linux Docker; plain ↔ ClickHouse on native
  Linux (no TimescaleDB packages). Backup first, switch SENTIENT (about a
  minute down), then copy history month by month while it runs, each month
  checked, re-runnable; the old history is kept and listed under "Kept after
  earlier changes" for removal. SPM streams the rows itself
  (`telemetry_move`) instead of SENTIENT's `--backfill-clickhouse`, which
  needs ClickHouse to reach PostgreSQL and has no reverse direction.
- **D. Safe update** — every engine: dry run (breaking changes need explicit
  acceptance), database copied inside PostgreSQL (`CREATE DATABASE …
  TEMPLATE`), files or image swapped, schema upgrade, health check (HTTP),
  automatic rollback of files/image and database on failure.

Headless: `--job install|migrate|resume-migration|update|backup|restore|
use-restored|uninstall|kept|remove-kept`
for servers without a desktop; on native Linux the window and headless runs
hand such jobs to a root copy of the program (pkexec, else `sudo -n`).

## Phases

| Phase | Content | Exit check |
|---|---|---|
| **G0** | Spike: GPUI Kit window with input, select, radio, checkbox, progress from a task, folder picker | Renders and responds on Linux (Xvfb + software Vulkan), the win11 VM, over Remote Desktop, and on the Windows 10 box |
| **G1** | New `app/` crate: shell, navigation, SENTIENT theme (teal `#187074` light / `#26a69a` dark, Roboto, 6 px radii), persisted state, engine progress → UI | App opens in all three modes with placeholder pages |
| **G2** | Manager mode: Status, Control, Logs, Update | Parity with the Tauri screens |
| **G3** | Install mode: the 12-page wizard on the existing engines, reboot-resume, settings catalog | Clean install on the VM and the Win10 box, all three flavours |
| **G4** | Backup / Restore screens | Parity with the Tauri screens |
| **G5** | Maintenance mode, Apps & features registration, packaging (single exe + offline payload), CI; delete `installer/` and `src-tauri/` | CI builds `SENTIENT-Setup-<ver>.exe` |

Then A–D, built once in GPUI.

## Pinned versions

`gpui-kit = "=0.7.3"` (pins `gpui-pre 0.3.9`). GPUI's API changes between
releases; upgrade deliberately, never by `cargo update`.

## Risks

- **GPU dependence.** GPUI renders on the GPU. Software fallbacks (WARP on
  Windows, lavapipe/llvmpipe on Linux) must be acceptable — this is what G0
  checks first.
- **API churn** — mitigated by the exact pin.
- **Accessibility** — GPUI Kit ships AccessKit roles; acceptable for an admin
  tool.
