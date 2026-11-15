# Changelog

Notable changes to the SENTIENT Platform Manager (SENTIENT Setup), release by
release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Each release section starts with a one-line summary: it heads the release
notes and is what the Platform Manager shows beside an update. The notes are
made from this file with `scripts/release-notes.py` (see its help). Releases
are tagged `v<version>`; a `v*` tag push builds the installers and publishes
the release. Once it is complete, `scripts/publish-spm-to-sentient-release.sh
<version>` publishes the same installers on the public download repository
(QuakeString/sentient-release, tag `spm-v<version>`, never its "Latest") and
refreshes that repository's Downloads table.

## [Unreleased]

### Added
- Every page works on a remote instance: with a server open, Status,
  Control, Logs, Update, Backup and restore, Platform settings, Migration,
  Repair and Gateway show and change that server. "Open" on an instance
  card lands on its Status. Installing a gateway and uninstalling SENTIENT
  are done on the server itself.
- Windows servers as remote instances, over Windows' own OpenSSH Server:
  the Platform Manager copies the released Windows build there (checked
  by its published SHA-256) and runs each job as a scheduled task, so it
  outlives the connection like on Linux.
- A sudo password for key sign-ins, kept in the secret store, for servers
  where sudo asks for one.
- Remote instances: SENTIENT on other computers, reached over SSH, on the
  new Instances page (below the menu) and in the top bar's instance
  switcher. Each server gets a card with its system logo, state, version,
  database size, free disk and last backup, and Start / Stop / Restart,
  Update SENTIENT, Back up now and Logs. Adding a server shows its key
  fingerprint to compare before anything is sent; a changed key is refused
  afterwards. The key passphrase or password is kept in the system's secret
  store, never in the instance list. The Platform Manager copies itself to
  the server (checked by SHA-256) and runs the work there detached, so a
  dropped connection does not stop an update; it reconnects and follows on.
  The top bar takes the open instance's colour and says "Remote". Linux
  and Windows servers.
- "What's new" before every update: the Update screen, the notice bar and
  Setup show the release notes of every version between the installed one
  and the new one, grouped as Added, Changed and Fixed, with what the update
  does to the installation (database change, how long SENTIENT stops).
  Setup carries the notes inside it, so this also works without internet.
- Changes a newer Platform Manager makes to an installation set up by an
  earlier one are listed after it updates ("2 changes for this
  installation") and applied on your click, with only those steps run;
  before, they were applied only by Repair. Status shows a quiet line while
  they wait.
- "Update from a file…" on the Update screen when the release page cannot be
  reached: a Setup program or a SENTIENT package copied to this computer.
- On the Stable channel, a line says when a release candidate is newer.
- Setup that finds an earlier SENTIENT install that is broken or half
  removed, with or without its installation record, shows what it found
  (each service's state, which port it holds, data folder sizes) and three
  ways out: "Remove it and install fresh" (with a backup first when the
  database can still be read, a confirmation tick, and a check that the
  ports are free afterwards), "Repair it" (keeps the databases; an install
  without a record is taken over first; a failed repair leads to the fresh
  install) and "Install fresh, keep the old database".
- Status shows who uses the database's disk in one bar (PostgreSQL,
  ClickHouse, other files, free), how fast the databases grow per day and
  how long the disk lasts at that rate; the largest tables with plain names
  and bars; ClickHouse's size before compression, its history per month,
  how long history is kept and whether it has broken parts; and "Back up
  now" beside the last backup. When the disk runs low, the warning offers
  the two ways to free space: a history limit and removing the database
  copies earlier updates left.
- Settings in five tabs instead of one list of 88 variables: Platform (log
  detail, analytics, licence server), Database (PostgreSQL sized Small /
  Medium / Large from this computer's memory, Custom for the exact
  numbers, and the time-series store's settings), Network and
  certificates (SENTIENT's ports, checked free, and the certificate files
  with a file picker), Data retention (audit log, notifications and events
  as plain choices) and Advanced (every variable, grouped and searchable,
  with its built-in value and a reset, plain names or variable names, and
  "Add a variable" for ones the list does not know). One bar says how many
  values changed and what applying restarts.
- Replace this instance's data with a backup, from this or another instance
  and from any database setup: the backup's summary shows first (when and
  where it was made, its SENTIENT version, database type, size and what it
  holds), then how it fits this installation (type converted, data brought
  up to this SENTIENT, disk needed) with the reasons it cannot be used
  (newer SENTIENT, not enough disk, wrong passphrase, damaged file). One
  run restores beside the live data, switches, and, when ticked, deletes
  the previous data only after SENTIENT answered on the restored data.
  Backups now record their SENTIENT version, schema version, database type
  and computer; older backups say "not recorded".
- A port held by another program names that program (for example
  "postgres.exe in C:\Program Files\PostgreSQL\16\bin") and offers the
  next free port with one click.

- A top bar in SENTIENT teal: the Platform Manager's icon, the instance
  that is open ("This computer"), a monitor of its processor, memory and
  free disk (amber when one runs high; a click opens Status) and a bell
  that counts what needs attention (updates, waiting installation changes,
  low disk).
- About and preferences, from the (i) at the bottom of the menu: the
  Platform Manager's version and update state with What's new, the update
  channel and how often to check, and support: its own activity log, a
  support file (logs, versions and settings in one file, without
  passwords) and the documentation.
- The theme is one icon at the bottom of the menu: Light, Dark or Follow
  system, remembered.
- Product and database logos beside their names: the Platform Manager,
  SENTIENT and the Sentient Gateway, and PostgreSQL, ClickHouse and
  TimescaleDB (Status, Setup's database choice, restore summary).
- Platform settings, System administrator: the system administrator
  accounts with their lockout state, "Reset password…" (new password twice;
  it works at once, the account is unlocked and its open sessions are
  signed out), "Unlock" and "Sign everyone out…". Data retention gains how
  long telemetry history and alarms are kept by default. All of it goes
  through SENTIENT's own administration command (sentient-admin, SENTIENT
  4.4.0.3 and later), which writes SENTIENT's audit log; the Platform
  Manager never writes passwords into the database. With an older SENTIENT
  the section says to update SENTIENT first.
- Backup and restore: back up with one click and no passwords to type
  (Everything, Configuration only, or Choose… for the full list; telemetry
  history all, last 90 days or none; optional passphrase), with the
  estimated size, time and free disk shown first. Every backup is read back
  and each part compared with its checksum before it counts as done; the
  file's SHA-256 is shown and saved beside it as `<file>.sha256`, so a copy
  on a USB disk or another site can be checked without the Platform
  Manager. Large backups are written as numbered parts (4 GB by default)
  and restored from the folder; PostgreSQL telemetry is stored month by
  month, as ClickHouse's already was, so damage costs one month (backup
  format 2; older backups still restore).
- Automatic backup on the same page: every day at a set time (a scheduled
  task on Windows, a systemd timer on Linux), keeping the last N, with an
  optional second copy on another disk or network folder.
- A list of the backups in the backup folder with size, how they were made
  and their last check, and Restore…, Check (every part compared without
  restoring, the damaged ones named) and Delete; "Add a backup file…"
  brings in one from another instance. Status warns when the newest backup
  is damaged.
- Windows: typing "SPM" in Start search finds the Platform Manager (its
  Start menu entry is now "SENTIENT Platform Manager (SPM)"; the old entry
  is replaced when it updates or is repaired), and `spm` works in the Run
  box. Not yet tried on a real Windows computer.

### Changed
- The menu ends with "Platform settings" (SENTIENT's settings); the
  Platform Manager's own options are under About and preferences. The
  version is shown once, at the bottom of the menu.
- Setup on a computer with SENTIENT shows a tick box for what it can update:
  the Platform Manager when this Setup is newer, SENTIENT when it carries a
  newer package. A line that cannot be updated is greyed out with the
  reason, and the button names what it updates ("Update both", "Update the
  Platform Manager", "Update SENTIENT"). Repair, Change database and
  Uninstall are under "Other choices". With nothing to update, Setup offers
  to open the Platform Manager.
- An older Setup no longer replaces a newer Platform Manager.
- Setup run where the Platform Manager was removed but SENTIENT is still
  installed installs only the Platform Manager (SENTIENT shows as
  detected, not touched) instead of opening Repair; the new Platform
  Manager says once what it found.
- The Update screen names the versions each choice moves between. With a
  new Platform Manager and a new SENTIENT, the Platform Manager goes first.
- "Update and restart" shows its steps and checks the download against its
  published checksum before replacing anything (before, only its size).
- Repair's database step reads "Checking the SENTIENT database (nothing is
  changed when it is already installed)" when the database exists, instead
  of "Installing the SENTIENT schema (one-time)".

### Fixed
- Setting up a remote server: the first copy of the Platform Manager
  failed ("stat remote: No such file or directory"), and a key with a
  passphrase or a password sign-in could not copy at all. A Linux server
  now gets the released Linux build (it runs on every supported
  distribution); the copy is checked to start there before it replaces
  anything.
- A remote server that is not set up shows one "Not set up yet" panel
  with Set up on every page, not shell errors.
- Instance cards: same height in a row, rounded corners all round, one
  Open button style; the summary names each state.
- Add and Edit instance open as a dialog, with a file chooser for the key
  and a colour picker; Setting up shows in a dialog with Try again.
- Linux: the window uses the desktop's font (KDE, GNOME, else fontconfig's
  sans-serif) instead of a fixed family the desktop may not have.
- Removing an earlier install also removes a PostgreSQL folder left by an
  install that stopped before its database was created.
- "Check for updates" finds new Platform Manager versions. It asked the
  Platform Manager's private repository, which answers nobody, so the
  manager never offered its own update; it now reads the public
  sentient-release page (the `spm-v…` releases). Release candidates show
  on the "Release candidate" channel.

## [0.8.0-rc11] - 2026-11-14

ClickHouse on Windows survives restarts, an earlier install is removed with one button, TimescaleDB only when chosen, and Status shows the databases.

### Added
- Status has a Database section: PostgreSQL's version, size, free disk (with
  a low-disk warning), connections, largest tables and the last backup; on
  PostgreSQL + ClickHouse installs also ClickHouse's version, disk use and
  stored telemetry history
  ([#14](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/14)).
- Setup finds what an earlier SENTIENT install left on Windows (its services,
  PostgreSQL and its data, ClickHouse in WSL2 with its hidden account and disk,
  an old Docker install's WSL distribution) and removes all of it with one
  button, "Remove the earlier install", on the System check page. It also
  stops ClickHouse's WSL machine, which kept port 8123 even after its account
  was removed.

### Fixed
- Windows: choosing PostgreSQL or PostgreSQL + ClickHouse no longer puts
  TimescaleDB into PostgreSQL. Before, every Windows install downloaded it,
  preloaded it and enabled it in the `postgres` database, whatever the
  choice; SENTIENT's own database was never affected. Now only the
  TimescaleDB choice installs it, and a later migration to TimescaleDB adds
  it at that point. An existing install that already preloads it is left
  as it is.
- Windows, PostgreSQL + ClickHouse: ClickHouse is stopped cleanly before
  Windows shuts down (a new "SENTIENT ClickHouse" service), writes its data
  through to disk, and starts even after an unclean stop left many empty
  parts. Before, a restart could leave SENTIENT unable to start
  ("Suspiciously many broken parts", TOO_MANY_UNEXPECTED_DATA_PARTS).
  Repair brings existing installs up to date: it adds the settings to their
  ClickHouse (which also gets a ClickHouse that already refuses to start
  going again, keeping all data), the clean stop and the new keep-alive.
- Uninstalling but keeping the database also stops ClickHouse and keeps it
  from starting at boot; it held ports 8123 and 9000, so a new install
  refused them.
- A new install over a kept database fills in that install's passwords and
  takes over its ClickHouse ports, and offers "Delete it" beside
  "Keep using it" and "Start fresh".
- The Platform Manager installed on its own offers "Download and install
  SENTIENT" (Setup downloads the version you pick) besides "Install from
  local files"; before, it only accepted a downloaded package folder.

## [0.8.0-rc10] - 2026-11-14

Choose which SENTIENT version to install or update to, operator screens that show SENTIENT right after a restart, a start delay you set, coloured logs, and a cleaner uninstall.

### Added
- Setup and the Update screen list the published SENTIENT versions to install
  or update to, newest first; going back to an older version is allowed when it
  runs on the database as it is, checked before anything changes
  ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).
- Start delay: how long SENTIENT waits after Windows starts, 10 seconds by
  default (0 starts it with Windows), set in Setup and changed later under
  Control → Start with Windows
  ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).
- Operator screens open at once on a "SENTIENT is starting…" page that shows
  SENTIENT as soon as it answers, and says what to check if it does not
  ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).
- Logs are coloured by level: errors red, warnings amber, debug and timestamps
  muted ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).
- `scripts/release-notes.py`: release notes made from a repository's
  CHANGELOG.md (`extract`, `body`, `check`, and `draft` from git history).
- `scripts/publish-sentient-release.sh --changelog <SENTIENT/CHANGELOG.md>`
  publishes the version's changelog section as the release notes, without PR
  links, with its downloads; the summary becomes the manifest's `notes`.
- The release manifest lists the SHA-256 of the server program for each
  platform and for the Docker image (`--docker-image`), so the license server
  knows each build ([#11](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/11)).

### Changed
- Update notices show the release's one-line summary instead of its whole
  notes; the full notes stay on the release page.
- SENTIENT's services start with Windows without Windows' two-minute delayed
  start; installs from earlier versions move to the 10 second start delay on
  update or repair
  ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).
- Status shows a service that is about to start with Windows as Starting, not
  Stopped ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).

### Fixed
- Open SENTIENT opens the full-screen (kiosk) window when the install chose
  the kiosk shortcut, and the normal browser otherwise; a kiosk always gets its
  own full-screen window
  ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).
- Uninstall removes the kiosk shortcuts (shared and per account), the start
  delay task and the kiosk's browser data
  ([#13](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/13)).

## [0.8.0-rc9] - 2026-11-02

PostgreSQL 18 no longer crashes during setup on computers with an old Visual C++ runtime.

### Fixed
- Native Windows install: Setup checks for Visual C++ runtime 14.44 or newer
  and installs it before PostgreSQL 18, whose `initdb` crashed on older
  runtimes. Workaround on older builds: install `vc_redist.x64.exe`, then
  Retry ([#10](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/10)).

## [0.8.0-rc8] - 2026-10-27

The Gateway card: install, update and control the Sentient Gateway from the Platform Manager.

### Added
- A Gateway screen and a card on Status: installed or not, version, running,
  the platform it sends to, and the newest gateway release on the update
  channel.
- Install the gateway with the platform address and MQTT port filled in from
  this SENTIENT, the gateway's access token and the local page's password:
  Windows runs `SentientGatewaySetup-<v>.exe` silently after checking its
  SHA-256; Linux runs the `quakestring/sentient-gateway-rs` container.
- Update the gateway in place, keeping its configuration, data and page
  password; the daily update notice also offers a newer gateway.
- Start, stop, restart, and Open local page (its address read from the
  gateway's own settings).
- A gateway container started from your own compose file is found and can be
  started, stopped and restarted; updating it is left to the compose file.
- Headless jobs: `gateway-status`, `gateway-install`, `gateway-update`,
  `gateway-control`.

## [0.8.0-rc6] - 2026-10-25

Update channels with a daily check and self-update, Keep or Start fresh for an earlier database, and port and uninstall fixes.

### Added
- Update channel (Stable, Release candidate, Nightly), Check for updates, and
  a daily check (on by default) that shows a notice when a newer SENTIENT or
  Platform Manager is out; Not now hides that version.
- The Platform Manager updates itself: it downloads its new installer and
  hands over to it.
- A new install that finds an earlier database asks: Keep using it, or Start
  fresh. Keep checks the history table against the chosen storage first and
  converts a plain table to TimescaleDB where it can; Start fresh sets the old
  database aside under Kept.
- Nightly Platform Manager builds at 02:00 IST on the `nightly` release.

### Fixed
- Windows: the chosen PostgreSQL port is written to `postgresql.conf`;
  PostgreSQL used to stay on 5432 while Setup waited on the chosen port.
- "Uninstall, keep the database" stops PostgreSQL instead of leaving it on its
  port, where the next install refused that port.
- Uninstall gives up on stuck steps (ClickHouse's WSL distribution after 3
  minutes, services after 1 minute) and says what was left.
- The reported version is the release tag; every release candidate used to
  report 0.8.0.

## [0.8.0-rc5] - 2026-10-24

Operator screens can sign in to SENTIENT automatically after a reboot.

### Added
- Optional automatic sign-in for operator screens ([#9](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/9)).
- `publish-sentient-release.sh --add` adds another system's files to a
  published release.

### Fixed
- The operator screen makes SENTIENT start with Windows.
- Setup says up front when SENTIENT's program files are not included.
- ClickHouse history set aside by a fresh install is named for what it is.

## [0.8.0-rc4] - 2026-10-24

SENTIENT downloaded from the releases site, an operator screen that opens SENTIENT after a reboot, and install progress.

### Added
- Setup downloads SENTIENT from the public releases site
  (QuakeString/sentient-release) and checks every file before use.
- Operator screens open SENTIENT after a reboot.
- Long install steps say what they are doing.

### Fixed
- Windows: a ClickHouse WSL disk left by an earlier install is reused on a
  reinstall over kept data, and set aside on a fresh install.
- Setup offers a download only when the release has files for this computer,
  and says so when there is no SENTIENT package.
- The online package line wraps inside its panel.

## [0.8.0-rc3] - 2026-10-24

Fixes from testing all eight install combinations.

### Fixed
- Windows: PostgreSQL checks passwords for local connections ([#8](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/8)).
- Windows: "Uninstall and delete" waits for PostgreSQL and reports leftovers.
- Native Linux: SENTIENT's ClickHouse user may create the database a restore
  needs.

## [0.8.0-rc2] - 2026-10-24

Add existing instance: manage a SENTIENT the Platform Manager didn't install.

### Added
- Add existing instance: take over a SENTIENT installed by hand or by another
  tool ([#7](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/7)).

### Fixed
- Linux: removing a pacman-installed Platform Manager goes through pacman
  ([#6](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/6)).

## [0.8.0-rc1] - 2026-10-22

The Platform Manager rebuilt on GPUI: one Setup program that installs SENTIENT with PostgreSQL, TimescaleDB or ClickHouse, then manages it.

### Added
- One Setup program that installs SENTIENT, its database and itself, and is the
  management app afterwards ([#2](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/2)).
- Storage choices: PostgreSQL, PostgreSQL + TimescaleDB, PostgreSQL +
  ClickHouse (in WSL2 on Windows).
- Safe update with rollback, and "Roll back last update" on the Update screen
  ([#5](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/5)).
- Backup and restore including ClickHouse, restore into another database
  type, migration, repair and uninstall ([#2](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/2), [#3](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/3)).
- Settings screen ([#3](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/3)).
- Install the Platform Manager alone ([#4](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/4)).
- Linux packages (.deb, AppImage, Arch) and GitHub Releases on tags ([#1](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/1)).

### Changed
- Ports in use are warnings, not blockers; Next is disabled until a step is
  complete; GNOME window frame ([#4](https://github.com/QuakeString/SENTIENT_Platform_Manager/pull/4)).

## Earlier versions

v0.2.0 to v0.6.6 (July 2026) were the Tauri Platform Manager. They were
tagged without release notes and their installers are no longer available.

[Unreleased]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc11...master
[0.8.0-rc11]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc10...v0.8.0-rc11
[0.8.0-rc10]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc9...v0.8.0-rc10
[0.8.0-rc9]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc8...v0.8.0-rc9
[0.8.0-rc8]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc6...v0.8.0-rc8
[0.8.0-rc6]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc5...v0.8.0-rc6
[0.8.0-rc5]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc4...v0.8.0-rc5
[0.8.0-rc4]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc3...v0.8.0-rc4
[0.8.0-rc3]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc2...v0.8.0-rc3
[0.8.0-rc2]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.8.0-rc1...v0.8.0-rc2
[0.8.0-rc1]: https://github.com/QuakeString/SENTIENT_Platform_Manager/compare/v0.6.6...v0.8.0-rc1
