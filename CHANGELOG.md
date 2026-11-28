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

### Fixed
- The wheel over a job's log (an update, a backup, a restore) scrolls the
  log only; the page around it no longer scrolls with it. The same for
  every other scroll box inside a page; a box with nothing to scroll lets
  the page scroll.
- A server's Status card no longer grows a few pixels while its server
  agent is checked or updated ("Updating"), or when the SENTIENT version
  check answers: those rows are as high with their pill as without.
### Changed
- Files is now just above Platform settings in the menu.

## [0.9.12] - 2026-11-27

Status shows whether a newer SENTIENT is available, on this computer and on a server.

### Added
- Status: the SENTIENT version shows "Up to date" or "<version> available"
  beside it, on this computer and on a server. The pill opens Update, so a
  newer SENTIENT no longer has to be looked for on the Update screen.

## [0.9.11] - 2026-11-23

Logs show each new line the moment it is written, on this computer and on a server.

### Changed
- Logs: Follow shows each new line the moment it is written, on this
  computer and on a server, instead of reading the last 500 lines again
  every 3 seconds. A server's lines come over its one SSH connection; no
  port is opened. Docker containers and the journal are followed by their
  own follow, log files are watched for new lines, and the few sources
  without either (the gateway's log folder on Windows, Docker inside WSL)
  are read again every 2 seconds on the server itself, sending only new
  lines. A server whose agent is older, or a follow that stops, falls back
  to reading every 3 seconds and tries following again a minute later.

## [0.9.10] - 2026-11-22

The Files page has Back, Forward and Up, a path you can type, and sorting by any column.

### Added
- Files: Back, Forward and Up beside each pane's path bar (Alt+Left,
  Alt+Right, Alt+Up; Backspace goes up). Each pane keeps its own history.
- Files: the path can be typed. Click the path bar's empty part or press
  Ctrl+L, type a folder (a full path, one under the folder shown, or `~`),
  Enter goes there, Escape cancels. A folder that is not there or cannot be
  read says so under the bar and the pane stays where it was; a server's
  path is checked on the server.
- Files: click a column header (Name, Size, Modified, Owner) to sort by it,
  again to reverse; an arrow shows which and which way. Folders stay above
  files; names sort naturally ("file2" before "file10"), case ignored. Each
  pane keeps its sort from folder to folder (Name, ascending, at first).

## [0.9.9] - 2026-11-22

Scrollbars are easier to see and stay clear of rounded corners.

### Fixed
- Scrollbars: the thumb is rounded at both ends and stays clear of a
  box's rounded corners; it is easier to see, and darker on hover and
  while dragged. There is no square track behind it any more.

## [0.9.8] - 2026-11-22

The gateway's connection can be set from the Platform Manager and works with localhost, a taken-over gateway keeps everything it had, Update in SENTIENT saves the connection there, and logs save with their progress.

### Fixed
- Gateway › Connection accepts localhost and 127.0.0.1 for a Docker
  gateway on its own network: it is written as host.docker.internal (this
  machine), the page keeps showing what was typed with a note, and Apply
  first checks SENTIENT's MQTT port answers there (published on all
  interfaces), then waits for the gateway to connect or puts the previous
  connection back. Every gateway container the Platform Manager creates
  (install, take over, update, Apply) gets host.docker.internal.
- Gateway › Connection on a Docker gateway: the new host was written
  while the gateway still ran, and a gateway writes its connection in
  force back to its file as it stops, so the new container started with
  the old host (and "connected" with it). The file is now written between
  stop and start, the gateway's own start line is checked against it, and
  the reachability check runs inside the gateway's container. When
  SENTIENT then sends the connection it keeps for the gateway, the job
  says so and where to change it. Messages never name the gateway's legacy
  file; a test gates it.
- A gateway install says whether the gateway connected to SENTIENT.
- When SENTIENT's saved connection replaces the one set on Gateway ›
  Connection and the gateway is connected with it, Apply ends as done with
  a note, not as a failure, and offers Update in SENTIENT: sign in once
  as the gateway's tenant administrator (the password is never stored,
  the session lasts until the Platform Manager closes) and the typed
  connection is saved in SENTIENT's gateway configuration, read back,
  and checked on the gateway.
- A native install (Windows, Linux) listens on the MQTT and CoAP ports
  chosen in Setup: they are written to SENTIENT's connectivity settings
  before its first start. Before, it always used 1883 and 5683.
- A database that refuses its password is said as such ("PostgreSQL
  refused the password for postgres"), not as a cancelled password prompt.
- A Docker gateway from 4.18.3 (it dials the machine itself for localhost
  in a container on its own network) gets localhost / 127.0.0.1 written
  as typed; an older one gets host.docker.internal. When SENTIENT's saved
  connection replaces the one set here, the job names it and what works.
- A native (systemd) or Windows service gateway: a new connection on
  Gateway › Connection counts only once the gateway connects with it
  within 60 seconds; otherwise the previous host and port are put back
  and the job says so.
- A failed job shows its own explanation first, with the last error
  lines under it, instead of the last log line; a long explanation is no
  longer replaced by "The action did not finish on the server".
- A remote install stops when the server has data an earlier SENTIENT
  left (its Docker volumes, /var/lib/sentient) without its containers or
  service, naming it, instead of installing over it.
- A job on a server that failed was shown as done (gateway install,
  update, Apply, take over, start/stop, remove; SENTIENT's update when it
  stopped early): its error went where the window did not read it. Every
  failure now reaches the window, and a job that ended badly without a
  word (killed, crashed) says it stopped.
- Gateway in Docker: its buffer and logs folder gets a volume when it had
  none (a take-over, an update or Apply created it again and lost them).
- Gateway in Docker: the web page on a container with its own network is
  published on this machine (this machine only or all interfaces, as
  chosen), no longer published when it is off, and Apply checks that the
  page answers. The page says how the gateway runs (Docker on its own
  network, Docker on the host network, a service).
- Gateway in Docker: taking over, updating or applying keeps every network
  and its names, a fixed address, a given command, user, privileged mode,
  labels, log driver, tmpfs and limits. A taken-over container keeps its
  old name on its network. An update keeps a config or data folder given
  for this container.
- Gateway: the connection to SENTIENT (host, MQTT port) can be changed from
  the Gateway page. Docker: written to the gateway's config file, and the
  container's environment no longer overrides it; a service: through the
  gateway's own --install. Not knowing whether it is connected no longer
  locks it.
- Setup keeps the settings chosen in it (PostgreSQL tuning, licence
  server, extra variables) in the installation's record: Settings shows
  them, and Apply, Repair or an update no longer put the defaults back.
- Setup with a kept database: when its administrator is no longer
  admin@sentient.local, Setup says the new sign-in was not set and the
  kept one stays; a new database that does not take it is an error.
- An install on a server that already has SENTIENT stops and says so, so
  its database is never reused or taken over by chance.
- Native Linux: SENTIENT's web, MQTT and CoAP ports are opened in firewalld
  or ufw when one is active, so other computers reach it.
- A server's gateway form starts with 127.0.0.1 (the same machine as
  SENTIENT), not an empty host that invited its public name.
- Passwords no longer show in the process list (root steps run from a
  private file; the schema installer reads DATABASE_URL from its
  environment; Windows registers its task through PowerShell with the
  password on standard input), and the files holding them are private: the
  Docker compose file (0600), and on Windows conf\sentient.env, the
  service descriptor and ClickHouse's config (Administrators and SYSTEM).
- Logs: Save log is a job with its progress and Cancel. The log goes into
  the file as it is read (a day of SENTIENT's log is never held whole); a
  server's log is written gzipped there and downloaded with its progress.
  It ends saying where the file is, or why it was not saved; a log that
  could not be read is never saved as if it were the log. The file is
  named after the log it holds (sentient-log-…, gateway-log-…).
- Logs: the log box has a scrollbar.
- Scrollbars: every scrolling area (pages, menus, dialogs, lists, the log
  box) shows its bar when the pointer comes near its edge and while it
  scrolls.

## [0.9.7] - 2026-11-21

A Platform Manager installed from the Arch package, the .deb or the .rpm updates itself, with the administrator password, and every job says what it is.

### Changed
- A Platform Manager installed from a package (the Arch package, the .deb
  or the .rpm) updates itself: Update downloads that kind of package from
  the release, checks its SHA-256 and signature, installs it with the
  system's package manager (pacman, apt or dnf/zypper) after the
  administrator password, and opens the new version. Before, it refused
  and said to update it with the package manager. A Flatpak still updates
  through Flatpak. Releases sign the .deb, .rpm and Arch packages.
- Every job has its name: the progress strip, the bell and a failed
  job's details say what ran ("Updating the Platform Manager"), never
  "A job".

### Fixed
- A job that stopped with the Platform Manager's own message and printed
  nothing else shows that message as it is: no "This is not a cause the
  Platform Manager knows yet", and no empty "No output yet." box under it.
- Administrator steps find the system's programs when sudo's own search
  path leaves them out (sudo-rs: "sudo: 'pacman': command not found");
  the package tools are called by their full path.

## [0.9.6] - 2026-11-21

Backup details show what a backup holds and check each of its parts, and the Backup page cards read Backups, Restores and Backup settings.

### Changed
- Backup and restore: the cards read Backups, Restores, Backup settings
  (the settings card was titled "Backup" and is now last). Restore from
  file is on the Restores card, where restores are, with a small History
  title over the list of past restores. The same on a server's page.
- Backup details: what the backup holds, from its own record. The file
  and its check at the top; where and with which SENTIENT, database and
  Platform Manager it was made, what was backed up, encryption and folder
  (a fact an older backup did not record says so); its data (rows and
  size per kind, or Not included); its parts with each one's last check;
  and the platform at backup time (address, certificate, licence,
  gateways; never a secret). Restore from it. An encrypted backup shows
  its contents only when restored with its passphrase. Checks now record
  each part's result; for a backup checked before that, the dialog offers
  Check now. Telemetry with no valid time (stored as 1970) is named so.
  One scroll area: the dialog's body, never a table inside it.

### Fixed
- Releases: the sideload MSIX is `SENTIENT-Platform-Manager-<v>-sideload.msix`,
  so it no longer collides with the Store package of the same name when a
  release is put together.

## [0.9.5] - 2026-11-20

Gateways started from a compose file can be taken over, new releases show without a restart, the Logs page fills the window, and the Windows payload has its own name.

### Fixed
- The "New" mark on Files shows for one day from the first time it
  appears, then not again. It stayed there for good before.
- Logs: the log box fills the window's height, here and on a server, and
  the page scrolls only when the window is too low for it. It had a fixed
  height before, with empty space under it on a tall window.
- New releases show without a restart. What a check found was kept until
  the next daily check while the window stayed open, so a gateway, SENTIENT
  or Platform Manager release published meanwhile was not offered, however
  often a page was opened or refreshed. It is now looked for again when a
  page opens more than 30 minutes after the last check, every 30 minutes
  while the window is open (with daily checks on), and at once with a
  server page's refresh button or its Check for updates (at most once a
  minute). A server's Check for updates read the server again but not the
  release list before.
- Gateway updates when GitHub limits requests: the release list is then an
  old one, so a gateway in Docker is also offered the newest image tag on
  Docker Hub when that is newer.
- A gateway container started outside the Platform Manager (by a compose
  file or by hand) can be taken over: "Take it over" on the Gateway page,
  here and on a Linux server. It is recreated as the Platform Manager's
  sentient-gateway with the same image, settings, volumes, network (and
  its compose service name there), ports and extra hosts; its data stays.
  Its compose service is commented out (the file as it was is kept as
  .spm-bak), or left as it is with a note on what to remove. If the new
  container does not start, the old one is put back as it was. From then
  on the Platform Manager updates it and changes its settings. "Manage it
  here" only recorded such a container: updates and settings stayed with
  its compose file, and the button was gone after it.

### Changed
- The Windows SENTIENT payload is `sentient-payload-windows-x86_64.tar`
  from SENTIENT 4.4.1.3 on, named like the Linux one. The Platform Manager
  looks for that name first and takes `sentient-payload.tar` when a
  release, an offline package or a package folder has only the old name,
  so every earlier release still installs. Releases carry the payload
  under both names for Platform Managers before 0.9.5.

### Added
- The Platform Manager can be published in the Microsoft Store. Windows
  then trusts it without a warning, and the Store keeps it up to date. The
  Store app works like the download: the same pages, the same UAC prompt,
  and Setup still continues by itself after the WSL restart. The
  differences: its Update page says "Updates come from the Microsoft Store"
  for the Platform Manager (SENTIENT and gateway updates work as before),
  it has no shortcuts or Apps & features entry of its own (the Store app
  has them), and it is uninstalled from Start or Settings > Apps.
  Uninstalling SENTIENT in the app also removes what the app added outside
  the Store app. The download from GitHub stays as it is. See
  docs/MICROSOFT_STORE.md.

## [0.9.4] - 2026-11-19

The gateway installs on remote Windows servers, the Platform Manager is code-signed by INVENIA SYSTEMS, Stable users get final releases, and Setup's progress, Windows ClickHouse disk use and self-update fixes.

### Added
- Install the gateway on a remote Windows server. "Install the gateway" on
  a Windows server's page (and on an empty Windows server) now works as it
  does on Linux: the same form, and the Platform Manager on the server
  downloads the gateway's Windows installer from the SENTIENT releases
  site, checks it against its published checksum, runs it silently and
  starts the SentientGateway service. The access token and the page's
  password reach the server on the agent's standard input and wait in a
  file only SYSTEM and the Administrators can read until the install takes
  them; they never appear on a command line or in a log. Installing
  SENTIENT itself on a remote Windows server is still not supported.

### Changed
- The Platform Manager is code-signed by INVENIA SYSTEMS, with our own
  certificate for now. 0.9.3 accepts an update signed with exactly that
  certificate; Windows shows "Verified publisher: INVENIA SYSTEMS" where a
  site trusts it (`Trust-INVENIA.ps1`), "Unknown publisher" elsewhere.
- The access token and passwords an install keeps for a moment on a
  Windows computer (a remote install's staged secrets, the gateway
  installer's token file) are readable only by SYSTEM, the Administrators
  and the account that wrote them, no longer by every user. A gateway
  release without a `.sha256` file is checked against the checksum GitHub
  records for its installer.

### Fixed
- Setup's progress bar no longer goes back across the restart that turns
  on WSL 2 (18 % before it, 4 % after): the run after the restart starts
  where the bar had got to.
- Logs on a computer or server with only the gateway (no SENTIENT) said
  "SENTIENT is not installed here". It now shows the gateway's log.
- The Stable channel offers a final Platform Manager release (0.9.3) even
  though the download repository marks it a pre-release; only a version
  with a suffix (0.9.4-rc1) is a release candidate. The release script
  publishes final versions as normal releases (never "Latest", which stays
  SENTIENT's) and -rc versions as pre-releases.
- ClickHouse on Windows no longer forces every write straight to the disk.
  Since 0.8.0-rc11 it wrote each new part and each merge through to the
  disk, which kept a hard disk 100 % busy. An update or Repair removes the
  setting from existing installs (a copy of the old file is kept). Set
  `SENTIENT_CH_FSYNC=1` to keep it on.
- Updating the Platform Manager no longer fails with "Access is denied"
  when its program file is still held for a moment or is read-only, and
  the ClickHouse shutdown service is started again after the update.

## [0.9.3] - 2026-11-19

Updates read Docker Hub for Docker installs and check the licence before a SENTIENT update, SENTIENT and the gateway install on Linux servers, native installs check glibc, and many Windows and gateway fixes.

### Added
- Updates follow how SENTIENT and the gateway are installed: a Docker
  install is offered a version only once its image is on Docker Hub, and
  falls back to Docker Hub's tags when the release list cannot be read; a
  native install needs its package and checksum on the release.
- Before a SENTIENT update the Platform Manager reads the licence's allowed
  versions (after a system administrator signs in; the password is never
  stored). A version the licence does not cover needs a typed confirmation,
  and the version list shows each version's licence label.
- Servers: Install SENTIENT and Install the gateway (with or without
  SENTIENT) on a Linux server, with the same steps as Setup. Remote install
  on Windows servers is not supported yet and says so.
- A native install checks the system's glibc against what the release
  needs; below it only Docker is offered, with the reason.

### Added
- Code signing with INVENIA SYSTEMS' own (self-signed) certificate, until a
  public one is bought. An update of the Platform Manager signed with exactly
  that certificate is accepted on Windows even though no computer trusts it
  by itself; any other untrusted signer is still refused. From 0.9.4 the
  Platform Manager is signed; the SENTIENT server programs, the gateway
  installer and the SENTIENT Client from their next releases.
  `Trust-INVENIA.ps1` (optional, for managed sites) makes Windows show
  "Verified publisher: INVENIA SYSTEMS".
- Gateway: the older Python gateway in Docker is offered an update to the
  Sentient Gateway ("Update available" on the Gateway page here and on a
  server, on the Status card and in the notices). The update backs up its
  configuration volume, stops the older container and keeps it aside,
  starts the new gateway on the same configuration with the same
  connection and identity, waits until it is connected to SENTIENT, and
  only then removes the older container. On any failure the new one is
  removed and the older one runs again as before. Its (i) lists what does
  not carry over (custom Python converters, Python code in a
  configuration, gRPC connectors, a file-storage backlog).
- Update page and Setup's update choices: a Sentient Gateway line, here
  and on a server. It shows the newer gateway release and runs the same
  gateway update with its progress; a gateway that cannot be updated from
  here is greyed with the reason. The older Python gateway's line carries
  the (i) on what does not carry over.
- Gateway in Docker: an update starts the container again with the
  volumes, host folders and connection settings it has, not fixed ones.
- Gateway: a settings card under the gateway's status, here and on a
  server, with the tabs Connection to SENTIENT, Local web page, Connectors,
  Storage, Logging, Statistics and Advanced, in the look of Platform
  settings. Every value is read from the gateway's own settings (as
  administrator for a native gateway, from its container and configuration
  volume for Docker, through the agent on a server); a token or password
  shows only as "set" or its last four characters. A gateway that has
  `--status-json` is asked for its state; an older one is read from its
  files.
  - What SENTIENT manages (connectors, storage, logging levels, statistics,
    remote shell, rate limits) is shown locked, with the reason and, when
    SENTIENT's address is known, "Open in SENTIENT". The connection is
    locked while the gateway is connected.
  - The status card says whether the gateway is connected to SENTIENT.
  - Local web page: on or off, where it listens, its port, and the
    administrator password (set through the gateway's own command, on its
    standard input).
  - Advanced: restart after a failure, serial ports (the service account's
    serial group, or a container's devices by their stable names) and
    environment overrides; Logging: the level before SENTIENT connects.
  - Apply runs as a job: the gateway's own `--install` and a drop-in of the
    Platform Manager's for a native service, the container created again
    with the same volumes and environment, or the Windows service's
    settings; then the settings are read back.
  - Test connection checks that SENTIENT's MQTT answers where the gateway
    connects (TCP, and a TLS handshake when it uses TLS). It does not try
    the gateway's credentials yet, and says so.

### Changed
- After quitting, the Platform Manager starts on Status, of the instance that was open last. Closing only the window (the Platform Manager keeps running in the tray) and opening it again still returns to the page it was on.

### Fixed
- Windows with "Start with Windows" off: opening Platform settings started
  SENTIENT's WSL distro. Its Data locations card read the Docker volumes
  through a path that skipped the read guard. Every command that runs in
  the distro now asks the guard first (a test scans the sources for any
  that does not), and the card says "SENTIENT is stopped. Where its data
  is kept is read when it runs."
- Platform settings with the distro up but Docker not started: the
  accounts showed a raw "failed to connect to the docker API … docker.sock"
  error. That is SENTIENT stopped: the plain sentence and Start SENTIENT.
  While a Start or Restart runs here the row says "SENTIENT is starting…"
  with no button, and the accounts are read only once SENTIENT answers.
- Statistics: the Services card said "stopped" for minutes after SENTIENT
  started. It is read again when a job here ends, and every 5 s while
  shown and stopped.
- Status: with Docker not started, Services read "0 of 0 running"; each
  container is now listed as stopped. After Stop, the Database card said
  "Could not read PostgreSQL … connection failed" instead of SENTIENT is
  stopped.
- Instances: "Up for" showed on a stopped instance; it shows only while
  SENTIENT runs.
- Files: the read-only banner says "You can view, but not change
  anything."
- The window closed and opened again from the tray (the Platform Manager
  kept running) showed the loading screen until the server or this
  computer was read again. It now shows the page it was left on with what
  was read last, at once, and reads it again behind it: Status and
  Statistics, on this computer and on a server, and the top bar's
  processor, memory and disk. Only a new start of the Platform Manager
  shows the loading screen. The reopened window also draws about 70 ms
  sooner (the backup tools are set up once per run, not for every window).
- Windows (a virtual machine without a GPU): Statistics took about 3 of 4
  processors, on a stopped or a running SENTIENT. The loading placeholders'
  shimmer asked for a new frame every frame even with motion reduced, and
  there every frame is drawn on the processor. The placeholders and the
  Refresh icon now stay still whenever motion is reduced: on a software
  renderer, and when "Show animations in Windows" is off (now read too).
- Statistics on Windows: the Server card stayed a loading placeholder for
  minutes. It waited for Windows Update's count of waiting updates, which
  can take minutes to search; that count is now searched for behind and
  shown at the next reading, and Windows' reading itself stops after
  60 s with a plain reason on the card instead of a placeholder.
- Statistics on Windows with Docker: the Services card never showed. Its
  `docker stats` ran on Windows, where there is no `docker`; it now runs
  inside SENTIENT's WSL distro, in one command, every 6 s instead of
  every 3. With SENTIENT stopped (its distro or every container) the card
  says "SENTIENT is stopped. Its services are read when it runs." on
  Windows and Linux alike.
- Platform settings: after Start SENTIENT, the General tab kept saying
  "SENTIENT must be running to read and change its settings." until the
  Platform Manager was restarted, because the accounts were read only
  when the page was first opened. They are now read again when a job
  here ends (Start, Restart, an update), and every 5 s while the page
  shows and SENTIENT was found stopped (started from the tray, Status or
  with Windows).
- Files on a server: opening a folder (double click, Enter, a path-bar
  segment or a place button) looked like it did nothing, because the
  page waited for the whole listing before showing anything, and a
  listing took 12 round trips (the folder's own attributes, the folder
  above it listed again only to read the owner, each link, one request
  at a time): about half a second on a 40 ms link. A listing is now one
  pipelined read of 2 round trips (3 with links), about 85 ms on the
  same link, and no new connection. Every click moves the path bar at
  once; until the folder's rows come, the list shows the loading rows
  the other pages use. Folders read in the last 45 s show at once and
  are read again behind; after a folder opens, its subfolders (up to 50,
  one at a time over the same connection, stopped by the next click),
  and when Files opens, the place buttons' folders and the folders
  above, are read ahead. The newest click wins; a folder that does not
  open puts the bar back on the folder before, with the reason under
  it. This computer's pane no longer lists every disk for each folder.
  `tests/files/run.sh` clicks each of these on both panes against a real
  server and checks the times.
- Platform settings on a Windows Docker installation: the system
  administrators and the default retention can be read and changed.
  The page ran `sentient-admin` on Windows, where it is not ("start
  sentient-admin: program not found"); it now runs in SENTIENT's
  container inside the WSL distro, as every other Docker command does.
  On Linux Docker it now goes through Docker's own shell too (`sg docker`
  right after setup). With SENTIENT stopped the page says "SENTIENT must
  be running to read and change its settings." with Start SENTIENT, and
  never starts the stopped WSL distro to ask.
- SENTIENT stopped (on Windows its WSL distro stopped): the Status page's
  Database card says "SENTIENT is stopped. The database is read when it
  runs." in plain text, not "Could not read PostgreSQL: read the stack's
  compose file: could not run wsl.exe". The same on a server's Status
  page, in Statistics' Services card ("Its services are read when it
  runs.") and in Backup's size estimate.
- Instances and the instance switcher show every server's state at once.
  The states are read for every added server as the Platform Manager
  starts, over the connections it opens then. They are read again every 15
  seconds while a window is open, and when the window is focused again.
  Before, a server was first read only when Instances or the switcher was
  opened, and its card and dot took a few seconds to appear. Each server's
  last state is kept on disk, so a new start shows it at once as
  "Checking…" until the server answers. A server that stops answering keeps
  its last state on the card, marked "Not reachable" with the time of its
  last answer. One request per server at a time; the tray's checks reuse
  these readings.
- Windows Docker: reading SENTIENT's state no longer starts its WSL distro.
  Any command for the distro starts it, and with it SENTIENT, so the Status
  page started SENTIENT about a minute after it was opened, also with
  "Start SENTIENT automatically with Windows" off. The Status page, the
  tray's checks, the statistics and their sampler, the instance cards and
  what another computer's Platform Manager reads now first look whether
  the distro is running; when it is not, SENTIENT shows as stopped and the
  distro stays stopped. Start, Restart, Repair and updates start it as
  before.
- Windows Docker: changing "Start SENTIENT automatically with Windows" in
  Platform settings no longer fails after a minute on a busy computer.
  Changing the boot task may take 3 minutes and is tried once more when it
  does not finish; the error names what did not finish.
- Gateway, Linux without Docker: the Update button says why it is off (a
  package-installed gateway updates with the system's package manager; one
  installed by hand with the new version's `--install`), instead of "comes
  from a compose file". The update job refuses the same cases.
- Gateway, Linux without Docker: the web page shows its real state (on at
  127.0.0.1:8642 by default, or what the settings and the service's
  environment say), not always "Off".
- Gateway, Linux without Docker: a gateway whose settings folder is closed
  to this account (`/etc/sentient-gateway`, 0700) no longer looks "Not
  commissioned". The page says the settings are readable by an
  administrator only and offers "Read its settings", which reads them as
  administrator (host, port and page only, never the token).
- Gateway on Windows: its logs show on the Logs page, from the service's
  log files (`ProgramData\Sentient Gateway\data\logs`).
- Gateway in Docker: the container the Platform Manager starts gets the
  serial adapters it finds (`/dev/serial/by-id/*`, else `ttyUSB*`/`ttyACM*`)
  with their group, so Modbus RTU works. An update passes them again.
- Gateway on a server: "Update to <version>" shows only when a newer
  gateway exists, names the version, and installs exactly that one.
- Gateway: a container of the older Python gateway is no longer taken for
  the Rust gateway (an update would have replaced it and lost its
  configuration).

- Windows "In the system": other computers reach SENTIENT. Setup opens its
  ports in Windows Firewall (rule group SENTIENT: web and MQTT over TCP,
  CoAP over UDP). Repair, a SENTIENT update and the Platform Manager's
  list of changes add the rules to an earlier install when one is missing.
- Windows Docker: "Start SENTIENT automatically with Windows" and its start
  delay now decide when the WSL distro starts: the task "SENTIENT Docker"
  starts at boot after that delay, or, when off, only when SENTIENT is
  started from the Platform Manager. Startup in Platform settings reads and
  changes the same task.
- Windows Docker: SENTIENT stays reachable after Windows restarts. WSL 2
  gives its VM a new subnet at every boot, which could overlap the stack's
  Docker network; Docker's networks now come from 10.214.0.0/16. Repair and
  the Platform Manager's list of changes move an earlier install's network.
- Windows Docker: with "Start SENTIENT automatically with Windows" off,
  Start on the Status page is offered after a boot (it was greyed while
  the WSL distro was not running) and starts the distro first.
- Windows Docker: every Start and Restart (the Status page, the tray,
  `control start`) points the forwarded web and MQTT ports at the WSL
  distro's address of now once the distro is up, also when the task
  "SENTIENT Docker" already runs, and is done only when SENTIENT answers
  on this computer's network address, the way other computers reach it.
  The task itself reads the distro's eth0 address and asks again while
  the distro starts, instead of leaving the ports unforwarded.
- Windows Docker: "SENTIENT answers again" and "SENTIENT is running" mean
  SENTIENT answers on its web port through the forward. They came as soon
  as its container was up, minutes before other computers could open it.
- On a computer without a GPU (a virtual machine), the Platform Manager
  draws without animations: the moving job bar and spinner made it
  redraw its whole window every frame, kept the processor near 100 % for
  as long as a job ran and slowed SENTIENT's own start beside it.
- Windows Docker: PostgreSQL's recommended memory is sized for the WSL 2
  VM (half the computer's memory, or .wslconfig's memory=), not the whole
  computer; with ClickHouse beside a Windows PostgreSQL, PostgreSQL is
  sized for what the ClickHouse VM leaves.
- On a virtual machine without a GPU, Windows' Microsoft Basic Render
  Driver now counts as drawing without a GPU too (it does not say so in
  DXGI's own flag): the job line and spinner stand still and the Platform
  Manager no longer takes several processors while a job runs.
- Windows Docker: the first start of WSL 2's virtual machine right after
  Setup installed the WSL 2 kernel can miss Windows' deadline ("a response
  was not received from the virtual machine or container"). Setup now
  stops WSL and creates the distro once more by itself instead of failing;
  when it does fail, the diagnosis says WSL did not answer in time and to
  Retry, not that WSL is missing. When WSL is missing, the command offered
  is the one this Windows takes (the Windows features and the WSL 2 kernel
  on an inbox WSL, `wsl --install --no-distribution` only where wsl.exe
  knows it).
- Repair and a reinstall over a kept database: the schema installer's
  "duplicate key … tb_user_pkey" is no longer a red error line in the job
  log. An existing database is the expected case; the log says "The
  database already has its data; kept as it is". Other schema installer
  errors stay red.
### Changed
- A new Docker install waits up to 10 minutes (was 6) for SENTIENT's first
  answer, and its log says every 15 seconds how long it has waited and
  what the container is doing.
- The Summary on Windows Docker shows CoAP as reachable from this computer
  only, with an (i) saying why (WSL 2 does not forward UDP).
- System check: "Check again" only on rows a new check can change, not on
  "Will be installed" rows.
- "Remove the earlier install?" has its buttons at the bottom right, as on
  the other Setup pages.

### Changed
- Pages switch instantly. The fade and rise on a page change are gone; dialogs and menus keep their open animation.
- Every popup opens with the same short animation: dialogs, the notifications and job panels rise 8 px as they fade in (180 ms); menus (the instance switcher, the theme menu, the licence menu, every dropdown and the Files context menu) and tooltips fade in moving 4 px toward what opened them (140 ms). Reduce motion turns them off. Dropdown and context menus, and plain tooltips, opened with none before.
### Added
- About and preferences (the (i) at the bottom of the menu) has an About
  block: the INVENIA SYSTEMS logo, the website and support address as
  links, this build (version, commit, build date, release or debug) and
  the copyright. Copy version info puts the build lines on the clipboard
  for a support request; Third-party licences lists the open-source
  components the Platform Manager is built with and their licences (a row
  opens the component's source). The list is made by
  `scripts/third-party-licences.py` from `cargo metadata`, and a test fails
  when Cargo.lock changes without it.
### Added
- Windows Setup offers Docker again: SENTIENT can run "On this computer"
  (Windows services, the default) or "In Docker" inside WSL 2, in the
  Platform Manager's own Ubuntu distribution with Docker Engine, started
  at every boot. Without WSL 2, Setup turns it on first (also on Windows
  10 builds whose wsl.exe cannot, through DISM and Microsoft's WSL
  package), Windows restarts once and Setup continues after sign-in. A
  computer that cannot run WSL 2 (virtualization off) shows the Docker
  choice greyed, with the reason.
- Setup's System check asks where SENTIENT runs, on Windows and Linux
  alike: "In the system" (Windows services, or systemd on Linux; the
  default and recommended on both, so Linux no longer starts on Docker)
  or "Docker" (inside WSL 2 on Windows). "Customise where each part runs"
  shows the place of the SENTIENT server, PostgreSQL (TimescaleDB goes
  with it) and ClickHouse; running them in different places comes in
  0.9.3, so those choices are greyed with an (i) for now. On Windows,
  "In the system" keeps ClickHouse in Docker inside WSL 2, as before.
  Each database card on the Database step says where it will run, and a
  database that can't be chosen is greyed with the reason in its (i)
  instead of hidden (TimescaleDB with systemd on Linux; ClickHouse on a
  Windows that cannot run WSL 2). Location, Database settings and the
  Summary's "Runs as" line follow the choice.

### Fixed
- Windows: the install no longer hangs when the PostgreSQL admin password is missing or not accepted. psql never waits on a password prompt now, so the install stops with a clear error instead.
- Windows: the window's minimise, maximise and close buttons were
  invisible on Windows 11 (only the red close button showed on hover), under
  an empty white caption strip above the top bar. There is no caption strip
  any more: the buttons sit at the right end of the top bar (Setup and the
  maintenance window: at the window's top right), drawn by the Platform
  Manager in the bar's text colour, crisp at every display scale, dimmed
  while the window is inactive, with Windows 11's hover (grey, red for
  close). The empty bar moves the window, a double click maximises it, and
  hovering maximise still opens the snap layouts.
- Docker: a SENTIENT on the image tag `latest` (or `dev`) showed "latest"
  as its version, and the Update page could not move it to a release.
  Status, the top bar and Update now show the version the container really
  runs (from the image's SENTIENT_BUILD_VERSION, else its version label),
  with the tag in brackets only when it differs: "4.4.0.2 (image tag
  latest)". Updates compare with that version, so a `latest` install that
  runs 4.4.0.2 is offered 4.4.0.3.
- Update, Other version: the running version is marked "(running)".
  Choosing a release on a Docker install now also writes that release's tag
  into the compose file (the update itself is unchanged: backup, pull,
  database changes, start and check), so the install stays on it. Choosing
  the version it already runs only changes the tag ("Pin to 4.4.0.3": one
  restart, no update and no database copy). A stack running from its own
  compose file keeps its tag until the compose file is taken over. Before,
  a chosen release ran under the old tag, so a numbered tag could name
  another release.
- New Docker installs use the tag of the release being installed, never
  `latest`.
- Docker roll back on the Platform Manager's own compose file names the
  release it went back to: after rolling back from 4.4.0.3, the compose file
  says 4.4.0.2 again (the release the previous image carries; an image that
  does not say its release keeps the tag), and :4.4.0.3 still names the
  4.4.0.3 image.
- "Update SENTIENT" on a Docker install pinned to a release updates to the
  newest release and pins it ("Update to 4.4.0.3", or "Up to date"), instead
  of pulling the tag it is pinned to again. `latest` and a compose file of
  your own still pull their tag.
- Windows: installing SENTIENT from the Platform Manager that Setup opened
  could stop at "initdb: program postgres is needed by initdb but was not
  found" (with "The current directory is invalid"). The Platform Manager
  ran in Setup's folder, which its administrator steps could not reach.
  The window now always runs in its own folder, every program it starts
  runs in a folder that exists, and this error is named with what to do.
- Windows: "PostgreSQL already present" now checks that its programs run
  (postgres -V) and puts them back from the archive when they do not; the
  data folder is never touched.
- The support details say "WSL is not installed" instead of wsl.exe's help.
- Files: double-clicking a folder did not open it when the pointer moved a
  little between press and release (rows can be dragged, and a drag
  swallowed the click). The second press now opens the folder. Alt+Up goes
  to the parent folder too (Backspace already did).
- Installing SENTIENT from the Platform Manager listed "Installing the
  Platform Manager" among its steps, although it was already installed:
  that step is now Setup's only.

## [0.9.1] - 2026-11-17

Add an existing SENTIENT on a Windows server however it was installed, a built-in terminal, restart and shut down servers, a Server card on Statistics, and Linux packages for Debian, Ubuntu, Fedora, AppImage and Flatpak.

### Added
- A built-in terminal: Statistics, Server card, terminal button. bash (or
  the account's shell) on a Linux server, PowerShell on a Windows server,
  a local shell for this computer, signed in over the connection the
  Platform Manager already keeps, with no extra password. Several terminals
  open as tabs; closing one that still runs asks first. Each session gets
  the Platform Manager's segmented prompt (system and user, then the
  folder) unless the account already uses a prompt theme (Powerlevel10k,
  Oh My Posh, Starship); nothing is written on the server. nano, htop and
  other full-screen programs work.
- Restart server and Shut down server for another computer (the Server
  card's buttons). Both ask first; shutting down needs the server's name
  typed. A restart is a job with its steps shown: it waits until the server
  really goes down, comes back (up to 10 minutes), has a new start time,
  and SENTIENT runs and answers again. Meanwhile the top bar says it is
  reconnecting instead of raising the connection-lost alarm.
- Statistics: a Server card in place of the Database card: machine and
  system, processor and memory, the disk holding SENTIENT's data, how long
  it has run and updates waiting, with amber lines only when something
  needs a look. All details opens every disk (health, wear, free space,
  unmounted ones), graphics, clock, network, updates and Docker.
- Linux packages beside the Arch one: Debian/Ubuntu (`.deb`, Debian 12+ and
  Ubuntu 22.04+), Fedora/RHEL/openSUSE (`.rpm`), an AppImage and a Flatpak
  bundle. Installed from a `.deb` or `.rpm`, the Platform Manager leaves its
  updates and removal to apt or dnf.
- The Flatpak is for managing SENTIENT on other computers (remote
  management); installing or managing SENTIENT on this same computer needs
  the .deb, .rpm, AppImage or Arch package. Its pages for this computer
  (Status, Setup and the rest) say so instead of looking empty.

### Fixed
- Windows servers whose SSH default shell is PowerShell now get the open
  connection (it was started the way cmd expects, so every request made
  its own connection there).
- Uninstall: a stopped PostgreSQL is "could not be checked", never "holds
  no other databases": PostgreSQL then stays unless you switch it on.
- The menu's selected row joins the page with curves at the pill's radius,
  one line colour in light and dark, drawn sharp at every scale.
- The SENTIENT logo tile's border is drawn on whole pixels at every scale.
- Windows without WSL: "WSL is not installed" instead of a line of
  wsl.exe's help.
- Server details name a graphics card's memory once.
- Windows: a SENTIENT running in Docker inside a WSL distro set up by hand
  (Ubuntu, …) was not found, on this computer or on a server through its
  agent. Add existing instance now looks in every running WSL distro of the
  account and in Docker Desktop, and offers what it finds ("Docker in WSL
  distro Ubuntu"), through the same add and take-over steps as on Linux.
- An instance added from such a distro keeps the distro's name in its
  installation record: status, logs, start and stop, updates and the rest
  run their Docker commands there. The Platform Manager never sets up,
  stops, removes or unregisters that distro; uninstall removes only
  SENTIENT's containers and volumes. Older records load unchanged.
- When nothing is found, Status (and Add existing SENTIENT on a server)
  says where it looked: the Windows services or systemd service, each WSL
  distro (a stopped one is named, not started), Docker Desktop, Docker on
  Linux.
- Adding such an instance on a Windows server showed only the progress
  bar: the job ran as SYSTEM, which cannot see a WSL distro (distros
  belong to one Windows account), and wsl.exe waited for ever. Jobs for an
  operator's WSL distro now run as the account the Platform Manager
  connects with: finding, adding, start/stop and logs in the connection's
  session, longer jobs (update, backup, uninstall) as a scheduled task of
  that account, so they still survive a dropped connection. Every WSL
  call now has a time limit, and a WSL that does not answer is reported
  with the account it ran as.
- Adding the SENTIENT found on a server said only "Adding it needs your
  choice: open Status", with nothing to choose from in the window on the
  other computer. It now says what was found and why it cannot be added,
  or what it needs.
- Copying the Platform Manager to a server could report "does not match
  (checksum)" when two copies ran at once (the check after connecting and
  a job's own); one copy runs at a time now, and a copy already in place
  counts as up to date.
- Windows: a Docker install made by an earlier Platform Manager (in its
  own WSL distro "sentient", stack in /opt/sentient) was not found when
  its installation record was gone: the Platform Manager's own distro was
  left out of the search. It is looked in now (started if stopped), and
  what it finds there is managed as the Platform Manager's own.
- A server's SENTIENT installed as Windows services by an early Platform
  Manager, with ClickHouse in WSL2 under its hidden account, could not be
  added from another computer: adding needs the PostgreSQL admin
  (postgres) password, which only a window on that server could ask for.
  The window that adds it now asks for the password itself (Enter adds;
  Set a new one lets the server give postgres a new password). It goes to
  the server once, on standard input, is checked there and kept only in
  that server's secret store; a wrong one shows the server's message and
  can be typed again. `--job add-existing --native
  --pg-admin-password-stdin` (and `add-if-one`) read it from standard
  input; add-if-one prints `@@NEEDS-PG-ADMIN <kind> | <location>` when it
  needs it.
- Windows services from releases before 0.7.0 read back with their own
  port variables (HTTP_PORT, MQTT_PORT, COAP_PORT); PostgreSQL's folder
  comes from its service's pg_ctl.exe, so a data folder on another disk no
  longer reads as "has no bin\psql.exe".
- The ClickHouse of such an install is checked over HTTP (`SELECT 1` on
  127.0.0.1 with SENTIENT's ClickHouse user), never with wsl as the
  connecting account, which cannot see the hidden account's distro.
  Whether it answers, and whether its task "SENTIENT ClickHouse" is
  there, is a note, never a reason not to add it.
- An installation record that no longer matches what runs on the computer
  (an early release's) no longer blocks adding: it is kept beside the new
  one as installation.json.replaced-<time>.

### Known issues
- The `.rpm` needs glibc 2.35 or newer: Fedora 39+, openSUSE Tumbleweed,
  RHEL 10; not RHEL 9.
- The Flatpak manages SENTIENT on other computers only (see Added).
- A SENTIENT added from Windows services shows its version as unknown
  until it is updated by the Platform Manager.
- Adding SENTIENT in Docker inside a WSL distro, and the Windows services
  with ClickHouse in WSL layout, were tested in unit tests and on the
  reporter's own server, not on a test machine with WSL.
- Restart server and Shut down server were tested in unit tests and in
  the dialogs and job view, not by restarting a real server.

## [0.9.0] - 2026-11-17

Manage SENTIENT on other computers, with errors that name their cause and fix.

### Added
- One open connection per server: every remote page reuses it (status
  4.8 s → 1.1 s, licence 7.6 s → 1.5 s on a 165 ms link); it reconnects by
  itself, and an older agent falls back to one call per request.
- One progress view for every long job (backup, restore, update, move data,
  repair, uninstall, Setup): all steps listed from the start, one bar, a log
  that follows the newest line, the result with its cause and fix.
- Uninstall: a switch for each part that is installed, "Keep its data" on by
  default, the space each frees, a line naming what goes and what stays, and
  a check afterwards that nothing is left.
- Setup's System check names what is in the way (a leftover PostgreSQL, the
  owner of a busy port) and offers the fix as a button, its command shown.
- Platform settings: one card with an Apply bar at its bottom, changes
  counted per tab, fields checked as typed, and a prompt before leaving.
- The server agent: the Platform Manager's job code without the window,
  built static for Linux (musl) and for Windows. Every package carries it,
  and Set up copies it to a server: it needs no desktop libraries and no
  particular C library there, and no published release to download.
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
- Action buttons sit on the right everywhere; labels say what they do (no
  trailing "…"); the menu's selected item is a pill drawn smooth at every
  scale; tooltips share one style in light and dark.
- Faster: the licence card shows its last reading at once, hidden pages stop
  polling, and the window opens about 1.6 s sooner when following the system
  theme.
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
- Windows: Stop keeps a PostgreSQL that other databases use running (it
  stopped it before), and listing those databases never waits on a password
  prompt.
- Windows: updating the agent on a server replaces the old one even while it
  runs (PowerShell 5.1 does not overwrite files).
- A failed job on a server shows its real error, not its last progress line.
- Native Linux asks for the administrator password once per job: Setup,
  Repair, Update and the other jobs start one administrator helper for all
  their root steps instead of one password prompt per step (Setup asked
  five times). A closed password prompt says so, with "Ask again".
- Viewing a page never asks for the administrator password. Setup and
  Repair give the installing account read access to SENTIENT's settings
  (group sentient-admin), so Status, Statistics and Licence read the
  database without root; until the next sign-in they say the values are
  available after signing in again.
- Statistics shows its first cards at once and never asks for a password.
  Processor, memory, disk, network and "Running for" come straight from
  this computer (a reader shared with the top bar, under 0.1 s after the
  page opens); Services and Database are read side by side and fill their
  own cards when they come. On native Linux, where the database's password
  is not readable yet, its numbers show "—" with a note (or "available
  after you sign in again") instead of the administrator password prompt
  that held the whole page and came back every few seconds; the
  sampler's timer no longer asks either. On a server, each part is its own agent call
  (`stats-live system|services|db`); an older agent's whole reading still
  works.
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

### Known issues
- A Windows server whose OpenSSH default shell is PowerShell does not use
  the one open connection; the Platform Manager calls it once per request
  (slower, but everything works). Fixed in 0.9.1.
- While a server's PostgreSQL is stopped, Uninstall cannot list the other
  databases it holds: leave "Keep its data" on there. Fixed in 0.9.1.

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
