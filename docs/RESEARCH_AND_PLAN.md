# SENTIENT Platform Manager — Plan

One desktop app to **install, run, update, and back up** the SENTIENT platform on
a machine. It unifies (and replaces) the former standalone **Installer** and
**Backup** apps. It reuses their engines verbatim — no rewrite.

## Decisions

- **Single unified codebase** in this repo. `SENTIENT_INSTALLER` and
  `SENTIENT_BACKUP` are retired once this covers them.
- Reuses both engine crates (copied here as the canonical home):
  - `sentient-installer-core` — preflight checks, WSL2, Docker Engine, deploy.
  - `sentient-backup-core` — DB backup/restore (selective, encrypted, TimescaleDB).
- **Rust + Tauri**, Windows-first (WSL2 + Docker Engine, no Docker Desktop).
- Installing the platform offers an **optional "install Client"** checkbox.
- The **SENTIENT Client** (end-user Tauri frontend) stays a SEPARATE app; the
  Manager can fetch+run its installer but doesn't contain it.

## Sections (unified UI — left nav)

1. **Setup** — the install wizard: System check → *Choose what to install*
   (Platform ☑, Client ☐) → WSL2 → Docker → Deploy → done. (from installer-core)
2. **Status** — is the stack running? start / stop / restart / view logs.
3. **Update** — `docker compose pull && up -d` to the latest image, with progress.
4. **Backup** — pick components + telemetry range, password-lock, write archive.
   (from backup-core)
5. **Restore** — create empty DB, restore an archive (incl. encrypted).
6. **Settings** — theme, saved connections (keychain), history, about.

Setup/Status/Update act on the **local** stack in the `sentient` WSL distro.
Backup/Restore can target the local stack OR any reachable SENTIENT DB (the
backup engine already connects over host/port), so the standalone-backup use case
is preserved as a section here.

## Architecture

- `src-tauri` depends on BOTH cores; its command layer is the union of the two
  apps' commands (checks/wsl/docker/deploy + inspect/backup/restore/store) plus
  new Status/Update commands.
- Windows: admin manifest (WSL/Docker need elevation), reboot-and-resume state
  machine (from installer), bundled pg_dump for Windows (from backup).
- Reuse the proven bits: Tauri progress Channels, off-main-thread commands,
  OS-keychain connection profiles, SQLite history, CREATE_NO_WINDOW, the
  Linux WebKit env workarounds.

## Phasing (built locally in batches; one CI build to test on Windows)

- **M0** — foundation: workspace + both cores + plan + CI + Tauri shell + nav.
- **M1** — Setup section: the install wizard (checks → components → WSL → Docker
  → Deploy), with the optional Client checkbox.
- **M2** — Backup + Restore sections (port the backup app UI + commands + store +
  bundled pg tools).
- **M3** — Status (start/stop/logs) + Update sections.
- **M4** — polish: icon, installer branding, finalize; then retire the old repos.

## Retiring the old repos

Only after the Manager does everything and is verified on Windows: delete
`SENTIENT_INSTALLER` and `SENTIENT_BACKUP` (GitHub deletion is the owner's to do).
The standalone Backup app's remote-DB use case is covered by the Backup section.
