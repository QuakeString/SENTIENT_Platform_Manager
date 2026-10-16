# SENTIENT Platform Manager

One program to **install, run, update and back up** the SENTIENT platform,
built with Rust and [GPUI](https://www.gpui.rs) (GPUI Kit components). It is
three things, depending on how it is started:

- **SENTIENT Setup** — `SENTIENT-Setup.exe` / `SENTIENT-Setup` on a machine
  without SENTIENT: one wizard installs SENTIENT, its database and the
  Platform Manager itself.
- **Maintenance** — setup run again where SENTIENT is installed (or
  *Uninstall* from Apps & features): update, repair, uninstall.
- **Platform Manager** — from the Start or application menu: status, start /
  stop, logs, update, backup and restore.

No third-party installer technology is involved: the program installs itself.
Design and phases: [docs/GPUI_PLATFORM_MANAGER_PLAN.md](docs/GPUI_PLATFORM_MANAGER_PLAN.md).

## What it installs

| Host                       | SENTIENT runs as                                    | Database (the operator chooses)                          |
|----------------------------|-----------------------------------------------------|----------------------------------------------------------|
| Windows 10 and later       | Windows services                                    | PostgreSQL · PostgreSQL + TimescaleDB · PostgreSQL + ClickHouse* |
| Linux                      | **Docker on this machine** (recommended)            | PostgreSQL · PostgreSQL + TimescaleDB · PostgreSQL + ClickHouse |
| Linux                      | natively with systemd                               | PostgreSQL · PostgreSQL + ClickHouse                     |

\* ClickHouse in Docker inside WSL2 — in progress.

On Windows SENTIENT always runs natively: its licence binding uses the
Windows hardware identity, which a container does not keep across restarts.
The wizard also takes every SENTIENT environment setting
(`app/assets/settings_catalog.json`) under *Customize*.

## Layout

- `app/` — the GPUI program (its own Cargo workspace).
- `crates/sentient-installer-core` — install engines: preflight checks,
  native Windows (PostgreSQL + TimescaleDB + SENTIENT as services), native
  Linux (systemd), Docker (on Linux, or inside WSL2 on Windows).
- `crates/sentient-backup-core` — selective, checksummed, optionally
  encrypted backup and restore.
- `scripts/package-windows.ps1`, `scripts/package-linux.sh` — build the
  offline SENTIENT Setup package (the program plus a `payload/` folder).
- `packaging/linux/build-payload.sh <sentient-repo> <out>` — builds the
  `sentient-payload-linux-<arch>.tar` the native Linux mode installs from.

## Building

```sh
cd app && cargo build --release
```

Linux needs the GPUI build libraries (Ubuntu: `libfontconfig-dev
libwayland-dev libxkbcommon-x11-dev libx11-xcb-dev libssl-dev libzstd-dev
libasound2-dev libvulkan1`). `gpui-kit` is pinned exactly; upgrade it
deliberately.

## Headless harnesses

The engines are UI-agnostic; `crates/sentient-installer-core` ships small
drivers for exercising them without the app:

- `sicheck` — the preflight checks.
- `sidocker <cmd> <config.json>` — the Docker engine (`compose`, `deploy`,
  `status`, `logs`, `control`, `update`, `cleanup`, `uninstall`); the JSON is a
  partial `DeployConfig`, missing keys take the reference defaults.
- `sinative <cmd> <root|->` — the native engine (Windows services or Linux
  systemd). `SINATIVE_FLAVOR`, `SINATIVE_LICENSE_SERVER_URL` and
  `SINATIVE_EXTRA_ENV` steer a test run.
