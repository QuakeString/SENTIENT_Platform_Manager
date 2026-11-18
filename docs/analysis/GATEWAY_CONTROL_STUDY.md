# Gateway control in the Platform Manager: study and proposal

Status: study, 2026-11-18. Owner's decisions of 2026-11-18 are recorded in
§0.1. Only the P0 fixes (§4.5) are being built (branch `spm/0.9.2-gw-p0`);
phases P1–P4 have no go yet.
Sources read: SPM `wt-rc4` (0.9.2-rc4, `4846928`), `sentient-gateway-rs`
(`eb5a596`, 4.16.2), `Sentient-Gateway` (the Python gateway, `4ff08a0`).
Paths in the tables are relative to those repositories: `core/` =
`crates/sentient-installer-core/src/`, `app/` = `app/src/` (SPM);
`gw/` = `sentient-gateway-rs/`.

---

## 0. Summary

- **Which gateway SPM manages:** the Rust gateway, `sentient-gateway-rs`,
  only (owner, 2026-11-18).
  - Docker image `quakestring/sentient-gateway-rs` (`core/gateway.rs:22`).
  - Releases are `gateway-<v>` tags on `github.com/QuakeString/sentient-release`
    (`core/gateway.rs:167-188`). Each release carries the Windows setup,
    `.deb`/`.rpm`/Arch packages and bare static binaries for x86_64, aarch64
    and armv7 (`scripts/publish-gateway-to-sentient-release.sh:8-11`,
    `gw/packaging/linux/build.sh:4-7`).
  - The Python gateway (`quakestring/sentient-gateway`) is not managed. A
    found one is **offered as an update** to the Rust gateway (§0.1, B13).
    Before 0.9.2 it was counted as a Rust gateway container
    (`core/gateway.rs:125-133`); its native installs are not detected.
- **Today SPM installs one shape per OS:**
  - Linux: Docker only (`core/gateway.rs:211-214`).
  - Windows: the native service only, through the gateway's Inno Setup
    (`core/gateway.rs:366-434`). A Docker install on Windows is refused
    (`core/distro/gateway_docker.rs:285-289`).
  - No native install on Linux, although the gateway ships packages and
    `sentient-gateway --install` for that.
- **Detection is broad:** Docker containers (ours or from a compose file),
  systemd units (the package's or hand-written), plain processes, and the
  Windows service.
- **Control is thin:**
  - Start, stop and restart work for everything except a plain process.
  - Update works only for the container SPM itself started and for the
    Windows service.
  - "Manage it here" only writes a record and changes nothing else
    (`app/gateway_record.rs:102-108`).
  - SPM can change **no setting** after the install. Host, port and token
    are asked once, at install time.
- **The coordinator's summary is right, with three corrections:**
  - A native Linux service can not only be started, stopped and removed:
    its *logs* are read too (`app/logsource.rs:65-87`).
  - The native update is not merely missing. The page shows it blocked
    with a wrong reason: "This container comes from a compose file"
    (`app/manager/gateway.rs:496-500`).
  - On Windows there are no gateway logs at all (`app/logsource.rs:83`).
- **The gateway already exposes most of what "complete control" needs:**
  - a CLI for installing, commissioning and setting the page password
    (`gw/bins/sentient-gateway/src/cli.rs:81-127`);
  - an authenticated local HTTP API: connection, configuration sections,
    connectors, restart, logs, TLS (`gw/crates/local-ui/src/api.rs:101-119`);
  - a "local edit" path that reports local changes back to SENTIENT
    (`gw/crates/gateway-core/src/remote_config.rs:551-...`).
- **The design rule that shapes the proposal:** in attached mode SENTIENT
  owns the gateway's configuration.
  - The shared attributes `general_configuration`, `storage_configuration`,
    `logs_configuration`, `active_connectors` and the connectors are
    re-pushed on every reconnect.
  - The on-disk mirror is overwritten by them
    (`gw/docs/INSTALL_AND_LOCAL_UI.md:162-196`, `gw/crates/gateway-core/src/mirror.rs:16-17`).
  - So SPM must **own only the machine-side settings**: how the gateway
    runs, where it connects before SENTIENT takes over, the local page and
    its password, the version, and Docker's own settings.
  - It must **route** platform-owned settings through the gateway's local
    edit path, or show them read-only, never as file edits.

### 0.1 Owner's decisions (2026-11-18)

- **Rust gateway only.** SPM supports and manages only `sentient-gateway-rs`.
- **Python gateway → an update.** A found Python gateway shows as "Update
  available: Sentient Gateway <Rust version>" on the Gateway page, the
  Update page and Setup's update choices, and runs the normal update flow.
  Inside, the update:
  1. backs up the Python gateway's config folder or volume (timestamped);
  2. installs the Rust gateway the default way (Native on Linux, the
     service on Windows; Docker when the Python one ran in Docker and native
     is not possible there);
  3. carries over the connection (host, port, credentials) and the
     connectors (the Rust gateway reads `tb_gateway.json`, `TB_GW_*` and the
     `thingsboard` root key, `gw/README.md:29`), listing beforehand what
     does not carry over;
  4. keeps the device identity (token / client id);
  5. starts the Rust gateway and waits until it is connected to SENTIENT;
  6. only then stops and removes the Python gateway;
  7. on any failure removes the Rust one and leaves the Python gateway
     running as before.
  It is never called a "migration" in the UI, the code or the docs. Not
  built in P0; it is its own piece of work after P0.
- **Default Linux install: Native** (package + systemd), preselected, with
  Docker as the alternative (answers open question 1).

---

## 1. Today: what SPM does with the gateway

### 1.1 Capability matrix

| Action | Linux, Docker (SPM-started `sentient-gateway`) | Linux, Docker (compose / other name) | Linux, native systemd | Linux, plain process | Windows, native service | Windows, Docker Desktop |
|---|---|---|---|---|---|---|
| Detect | yes (`core/distro/gateway_docker.rs:96-102,139-181`) | yes, shown as "installed outside" | yes (`core/distro/gateway_native.rs:106-129`) | yes (`:150-157`) | yes (`core/gateway.rs:328-349`) | yes (`core/gateway.rs:195`) |
| Install | **yes**, `docker run` (`gateway_docker.rs:284-316`) | n/a | **no** | no | **yes**, silent Inno Setup (`gateway.rs:366-434`) | **no** (refused, `gateway_docker.rs:285-289`) |
| Update | yes, recreate on the new tag (`gateway_docker.rs:266-282`) | no: tells you to edit the compose file (`:269-272`) | **no** (`gateway.rs:227-230` goes to Docker → "no gateway container") | no | yes, the installer again (`gateway.rs:221-223`) | no |
| Start/Stop/Restart | `docker start/stop/restart` (`gateway_docker.rs:318-333`) | same | `systemctl` (`gateway_native.rs:309-327`) | no (`:314-319`) | `sc.exe` with a 30 s wait (`gateway.rs:436-465`) | `docker` |
| Status: version | `--version` in the container, else the tag, else a throwaway probe (`gateway_docker.rs:150-156,197-246`) | same | `--version`, else the package (`gateway_native.rs:165-172,191`) | `--version` | `--version` (`gateway.rs:334-335`) | same as Docker |
| Status: host/port | container env `host`/`port`, else the file read by `docker exec cat` (`gateway_docker.rs:159-171`) | same | `/etc/sentient-gateway/...` files (`gateway_native.rs:24-29,183-185`) | same | `ProgramData\...\st_gateway.json` (`gateway.rs:323-326,342-343`) | same as Docker |
| Status: page address | from the file and env (`gateway.rs:87-123`) | same | **never set** (see bug B2) | never set | from the file and the service's registry Environment (`gateway.rs:346-347,352-362`) | same as Docker |
| Logs | `docker logs` (`app/logsource.rs:144-145`) | same | journal (`:146`) | no | **no** (`:83`, `!cfg!(windows)`) | **no** |
| Remove (keep data or delete) | yes, with volumes and the image (`gateway_docker.rs:340-400`) | yes; the compose file is left alone | yes: the package manager, or the unit and program; data moved to `/var/backups` (`gateway_native.rs:221-307`) | only when stopped (`:236-238`) | yes, `unins000.exe`; data moved to a dated folder (`gateway.rs:470-514`) | yes |
| "Manage it here" (adopt) | n/a (already managed) | records only (`app/gateway_record.rs:102-108`) | records only | records only | not offered (`app/manager/gateway.rs:407`) | records only |
| Change settings | **no** | no | no | no | no | no |

Remote servers (the agent over SSH):

- Headless jobs: `gateway-status`, `gateway-install <host> <port>`,
  `gateway-update`, `gateway-control`, `gateway-remove`, `gateway-adopt`
  (`app/headless.rs:279-345`).
  - `gateway-install` takes its token in `SPM_GATEWAY_TOKEN` and the page
    password in `SPM_GATEWAY_PAGE_PASSWORD` (`:331-341`).
- The remote Gateway page offers: status, Start/Stop/Restart, Logs,
  Remove, "Manage it here", and "Update to the newest"
  (`app/manager/remote_pages.rs:2369-2476`).
- It offers **no Install**. That is deliberate: "Install it with the
  Platform Manager on the server itself, so its access token stays on the
  server" (`remote_pages.rs:2400`).
- "Update to the newest" shows whenever the gateway can be updated, with
  no version comparison (`:2437,2472-2474`). The local page compares
  versions first (`app/manager/gateway.rs:401-403`).

### 1.2 What the install asks

- Host, port and access token (`app/manager/gateway.rs:108-136`). The host
  defaults to `127.0.0.1` and the port to the local SENTIENT's MQTT port
  (`:49-52`).
- **Windows only:** the local page password, typed twice
  (`:123-132,555-562`).
- On Linux Docker the container is started as
  `docker run -d --name sentient-gateway --restart unless-stopped --network host`
  (`gateway_docker.rs:296-306`), with:
  - three volumes: `st-gw-config`, `st-gw-data`, `st-gw-logs` (`:18-22`);
  - three env variables: `host`, `port`, `accessToken`.
  - Nothing else: no page password, no `ST_GW_UI`, no serial devices, no
    `group_add`, no `RUST_LOG`.

### 1.3 Gaps and bugs found

| # | Finding | Where |
|---|---|---|
| B1 | **Native Linux update is shown blocked with the wrong reason.** `blocked = !status.managed …`. A native status never sets `managed`, so the tooltip says "This container comes from a compose file". After "Manage it here" it is still blocked, because `recorded` is not consulted. | `app/manager/gateway.rs:496-500`; `core/distro/gateway_native.rs:193` |
| B2 | **A native Linux gateway's page always shows "Off"**, with the hint "In a container it is turned on with ST_GW_UI=on". `gateway_native::status` never calls `page_url`, although a native gateway's page is **on** by default at `127.0.0.1:8642`. | `core/distro/gateway_native.rs:187-207`; `gw/crates/gateway-core/src/config.rs:536-554` |
| B3 | **A native Linux gateway looks "Not commissioned" to an unprivileged SPM.** `/etc/sentient-gateway` is `0700 sentient-gateway` (`gw/packaging/linux/tmpfiles.conf`), so the GUI cannot read `st_gateway.json`, and host and port come back None. The page then says "Not commissioned (set the platform on its local page)". | `gateway_native.rs:183-185`; `app/manager/gateway.rs:433-437` |
| B4 | A unit started with `--config /some/other.json` is detected, but its host and port are read from the default paths only. | `gateway_native.rs:24-29` (the test at `:366` shows such a unit) |
| B5 | The program that `sentient-gateway --install` writes, `/usr/local/lib/sentient-gateway/sentient-gateway`, is missing from `PROGRAMS` and from `CHECKS`. A unit finds it through ExecStart. A stopped install without the unit is not found. | `gateway_native.rs:33-38`; `core/gateway.rs:27`; `gw/bins/sentient-gateway/src/install/linux.rs:85-87` |
| B6 | No gateway logs on Windows, neither for the service nor for Docker Desktop. The gateway writes rotating files under its data directory (`gw/crates/gateway-core/src/logging.rs:1-14`). | `app/logsource.rs:83` |
| B7 | The Docker install takes no page password, and the page is off in a container. Turning it on (only by recreating with `ST_GW_UI=on`) leaves the default `admin`/`invenia`, which signs in "from the gateway machine only" (`gw/docs/INSTALL_AND_LOCAL_UI.md:223-226`). | `app/manager/gateway.rs:555`; `gateway_docker.rs:298-305` |
| B8 | **The SPM-started container can never reach a serial port.** It has no `devices:` and no `group_add`, so Modbus RTU and other serial connectors cannot work in it (`gw/docs/INSTALL_AND_LOCAL_UI.md:57-78`). | `gateway_docker.rs:298-305` |
| B9 | **The Docker install's connection is frozen in the container's env.** The env overrides the file (`gw/crates/gateway-core/src/config.rs:1090-1236`). The local page then shows the connection read-only (`INSTALL_AND_LOCAL_UI.md:218-222`). An update re-reads those three env values and re-applies them (`gateway_docker.rs:276-281`). Changing host or token means recreating the container, which SPM does not offer. | as cited |
| B10 | The token is visible to anyone who can run `docker inspect`, because it sits in the container env. The gateway itself says a token belongs on stdin or in a file (`cli.rs:39-52`). | `gateway_docker.rs:301` |
| B11 | Remote "Update to the newest" is always offered, even when the gateway is up to date. | `app/manager/remote_pages.rs:2472-2474` |
| B12 | The Windows status says `managed: true` for any `SentientGateway` service running our program, including one installed by hand. The local page then never offers "Manage it here" on Windows. Harmless, but inconsistent with Linux. | `core/gateway.rs:348`; `app/manager/gateway.rs:407` |
| B13 | **Python gateways.** A Python container was counted as "ours" (`is_gateway_image`), so a container named `sentient-gateway` would have been recreated on the Rust image with the `st-gw-*` volumes and its Python config left behind. P0 (0.9.2): a Python container is no longer the Rust gateway; the page names it in one line. Decision (§0.1): it is **offered as an update** to the Rust gateway, built after P0. Native Python installs (`thingsboard-gateway` unit, Windows `SentientGatewayService.exe`) are not detected yet. | `core/gateway.rs:125-133`; `gateway_docker.rs:266-282`; `gw/bins/sentient-gateway/src/install/windows.rs:56` |
| B14 | "Manage it here" changes nothing: not the update path, not the controls. Only the "Runs as" wording changes (`app/manager/gateway.rs:367-378`). | `app/gateway_record.rs:102-108` |
| B15 | Docker `control()` returns as soon as `docker start` succeeds, with no wait for the gateway to be up. The Windows path waits up to 30 s. | `gateway_docker.rs:326-332` vs `gateway.rs:456-464` |

---

## 2. The gateway's settings surface (Rust gateway 4.16.2)

### 2.1 Where things live

| | Configuration dir | Data dir (queue, registry, logs) | Main file | Override |
|---|---|---|---|---|
| Docker | `/sentient_gateway/config` (volume `st-gw-config`) | `/sentient_gateway/data` (`st-gw-data`); logs in `/sentient_gateway/logs` (`st-gw-logs`) | `st_gateway.json` | `ST_GW_CONFIG_DIR`, `ST_GW_DATA_DIR` (`gw/Dockerfile` ENV) |
| Linux native | `/etc/sentient-gateway` (0700) | `/var/lib/sentient-gateway` (0750) | `/etc/sentient-gateway/st_gateway.json` | the same env, or a unit drop-in (`install/linux.rs:100-131`) |
| Windows native | `%ProgramData%\Sentient Gateway\config` (private ACL) | `%ProgramData%\Sentient Gateway\data` | `...\config\st_gateway.json` | the service's registry `Environment` |

Sources: `gw/docs/INSTALL_AND_LOCAL_UI.md:40-55`, `gw/crates/gateway-core/src/paths.rs:7-8,99-113`.

- An existing Python `tb_gateway.json` keeps its name (`INSTALL_AND_LOCAL_UI.md:327`).
- Connector files sit beside it, one `<connector>.json` each, with
  timestamped copies under `backup/` (`mirror.rs:3-5`).
- `local_edits.json` holds the pending local edits (`remote_config.rs:135`).
- `local-ui/admin.argon2` holds the page password hash
  (`gw/packaging/windows/sentient-gateway.iss:217`).
- `.credentials` holds provisioned credentials (`INSTALL_AND_LOCAL_UI.md:227-229`).

### 2.2 Settings, who owns them, and how a change applies

"SENTIENT" in the Owner column means the platform pushes this value as a
shared attribute on every connection. A file edit is overwritten by that
push (`mirror.rs:16-17`). A local change survives only through the
local-edit path (§2.4).

| Group | Setting | File key (`st_gateway.json`) | Env | Owner | Applying a change |
|---|---|---|---|---|---|
| Connection | Platform host | `sentient.host` | `host`, `ST_GW_HOST`, `TB_GW_HOST` | **SPM/local until connected, then SENTIENT** (`general_configuration`) | file: picked up live while *not commissioned* (`gw/bins/sentient-gateway/src/main.rs:214`); local API `POST /api/connection` moves the session with no restart, only while not connected (`INSTALL_AND_LOCAL_UI.md:188-197`); env: restart (Docker: recreate) |
| | MQTT port | `sentient.port` | `port`, `ST_GW_PORT` | same | same |
| | Credentials: access token, username/password/clientId, TLS token + CA, X.509 cert + key | `sentient.security.{type,accessToken,username,password,clientId,caCert,cert,privateKey}` (`config.rs:49-103`) | `accessToken`, `type`, `username`, `password`, `clientId`, `caCert`, `cert`, `privateKey` (+ `ST_GW_*`/`TB_GW_*`) (`config.rs:1138-1224`) | same; a provisioned gateway keeps them in `config/.credentials`, read-only | as host |
| | QoS, keep-alive, last will, proxy | `sentient.qos`, `keepAliveSeconds`, `lastWill`, `proxy_host/port` | `ST_GW_PROXY_HOST/PORT` | SENTIENT | push: live |
| | Provisioning (device key/secret) | `sentient.provisioning` (`config.rs:420-432`) | `TB_GW_PROVISIONING_*` | install-time | restart |
| Local web page | On/off | `localUi.enabled` | `ST_GW_UI=on/off` (env wins) | **machine (SPM)** | read at boot: **restart** (`config.rs:536-554`); CLI `--install --ui/--no-ui` |
| | Listen address and port | `localUi.listen` (default `127.0.0.1:8642`; in a container `0.0.0.0:8642`) | `ST_GW_UI_LISTEN` | machine | restart; anything other than loopback is HTTPS only |
| | Extra hostnames (certificate names) | `localUi.hostnames` | | machine | restart |
| | Admin password | `config/local-ui/admin.argon2` | `ST_GW_UI_PASSWORD_MIN_LENGTH` | machine | `sentient-gateway --ui-password -` (stdin), no restart; all browsers signed out (`cli.rs:120-122`); or `POST /api/password` |
| | Certificate | under config | | machine | `POST /api/tls`; renewed daily if due |
| Connectors | List and on/off | `active_connectors` (shared attribute) | | **SENTIENT** | push: live, per connector (`remote_config.rs:1-16`) |
| | Each connector's config | `<name>.json` mirror | | **SENTIENT** | push: live; a local edit is reported via `localConfigurationEdits` |
| Storage | Type (memory/file/sqlite), path, limits, TTL | `storage.*` (`config.rs:278-317`) | | **SENTIENT** (`storage_configuration`) | push: live engine swap (`remote_config.rs:1207`) |
| Logging | Levels per logger | `logs.json`-shaped `loggers.<name>.level` (`logging.rs:15-20`) | `RUST_LOG` (Docker default `info`), `TB_GW_LOGS_PATH` | **SENTIENT** (`logs_configuration`, `RemoteLoggingLevel`) | push: live |
| Statistics | enable, period, custom commands | `sentient.statistics.*` (`config.rs:204-240`, keys `:1021-1047`) | | SENTIENT (`general_configuration`) | push: live (`remote_config.rs:1129-1135`) |
| Device behaviour | device filtering, inactivity check, report strategy, renaming, sync shared attributes | `sentient.deviceFiltering`, `checkingDeviceActivity`, `reportStrategy`, `handleDeviceRenaming`, … | | SENTIENT | push: live |
| Remote features | `remoteShell` (off by default), `remoteConfiguration` | `sentient.remoteShell`, `.remoteConfiguration` (`config.rs:1013-1014`) | | SENTIENT | push: live. `remoteConfiguration` is **parsed but never read**: the gateway always obeys remote config (no other use of `remote_configuration` in `gw/crates`, `gw/bins`) |
| Rate limits, payload size | `sentient.rateLimits…`, `maxPayloadSizeBytes`, `minPackSendDelayMS` | | `TB_GW_RATE_LIMITS`, `…DP_RATE_LIMITS` (`config.rs:1243-1260`) | SENTIENT | push |
| gRPC | `grpc` | | | SENTIENT | not supported by this gateway (`remote_config.rs:996`) |
| Process | Restart policy, user, capabilities, serial devices | Docker run flags; the systemd unit (`gw/packaging/linux/sentient-gateway.service`); the Windows service | | **machine (SPM)** | recreate the container / `systemctl edit` drop-in / `sc config` |
| Version | | image tag / package / exe | | machine | update |

### 2.3 Docker vs native for the same settings

- **Docker:** host, port and token come from the env as SPM starts it
  (B9).
  - Every machine-side change (`ST_GW_UI`, `ST_GW_UI_LISTEN`, devices,
    group_add, `RUST_LOG`) is a **recreate** with the same volumes.
  - The page password can be set with
    `docker exec -i sentient-gateway sentient-gateway --ui-password -`. That
    is the same binary and CLI. Writing the config volume as uid 65532 still
    has to be verified.
- **Native:** the same keys live in the file. `sudo sentient-gateway --install …`
  re-run on an installed box re-commissions it in place. It stops the
  running gateway, writes, then starts it
  (`install/mod.rs:1-11`, test `a_reinstall_stops_the_running_gateway_first`, `install/linux.rs:691`).
  - It also handles the packaged unit, through a drop-in (`install/linux.rs:718-765`).
  - This makes `--install` the safe "apply connection and page settings"
    primitive on Linux and Windows. On Windows the Inno Setup passes the
    same flags (`sentient-gateway.iss:448-474`).

### 2.4 What SENTIENT already manages remotely, and how not to fight it

- SENTIENT's gateway configuration page (the extension) writes the shared
  attributes: `general_configuration`, `storage_configuration`,
  `logs_configuration`, `grpc_configuration`, `RemoteLoggingLevel`,
  `active_connectors`, `mode`, and one attribute per connector
  (`remote_config.rs:256-275`).
- The gateway applies each push live, echoes it back as a client attribute
  for the page's SYNC state, and mirrors it to disk.
- **Connection keys** (`host`, `port`, `security`, `proxy_*`) are also part
  of `general_configuration`, so the platform co-owns the connection.
  - The gateway trials a pushed connection once per save
    (`INSTALL_AND_LOCAL_UI.md:198-203`).
  - The local edit path never changes them (`remote_config.rs:141-144`).
- **Local edits** go through the local API, `POST /api/config`, which
  calls `apply_local`.
  - They apply like a push, stamped and kept in `local_edits.json`, and
    are reported to SENTIENT as client attribute `localConfigurationEdits`
    with their values (`remote_config.rs:513-549`).
  - "Last save wins": a newer save in SENTIENT undoes them.
  - **Neither SENTIENT nor the extension reads `localConfigurationEdits`
    today.** `git grep` finds it in neither repo. A local edit therefore
    lasts only until someone saves that section in SENTIENT.

The rule for SPM follows from this:

| Kind | SPM's role |
|---|---|
| Machine-side (process, page, password, TLS, version, Docker flags, unit drop-ins) | **Owns** it. Edit and apply. |
| Connection (host, port, credentials) | **Edits it only while the gateway is not connected to SENTIENT, or is not commissioned.** When connected, read-only with "Change it in SENTIENT › Gateways › Configuration". The gateway enforces the same rule. |
| Platform-owned (connectors, storage, logging, statistics, device behaviour, remote shell) | **Read-only by default**, shown from `GET /api/config`. An optional "Edit here" goes through the gateway's local edit path. It needs SENTIENT to take up `localConfigurationEdits` before it is safe (open question Q3). |

---

## 3. Native vs Docker per platform

| | Linux x86_64 (systemd) | Raspberry Pi / arm64, armv7 | Windows 10/11, Server |
|---|---|---|---|
| **Native install** | `.deb`/`.rpm`/Arch from the release (unit `/usr/lib/systemd/system/sentient-gateway.service`, user `sentient-gateway`, `package-setup`), then `sentient-gateway --install --host --port --token -` to commission. Or the bare `tar.gz` + `--install`, which copies to `/usr/local/lib/sentient-gateway/` and writes `/etc/systemd/system/sentient-gateway.service` (`install/linux.rs:81-87`) | the same packages for `arm64`/`armhf` (`build.sh:4-7,36`); static musl binaries, 23–24 MB, no runtime (`INSTALL_AND_LOCAL_UI.md:286`). **The better default on a Pi**: no Docker daemon | `SentientGatewaySetup-<v>.exe /VERYSILENT /HOST /PORT /TOKENFILE /UIPASSWORDFILE [/NOUI]` (today's path) |
| **Docker install** | today's path | only if the image is multi-arch: **not verified** (Q5); the Dockerfile has no platform logic | Docker Desktop / WSL: possible in principle, refused by SPM today. Serial ports cannot pass through without `usbipd` (`INSTALL_AND_LOCAL_UI.md:75-77`), and Docker Desktop's host network differs. **Recommend: not offered** |
| **Update, native** | the package manager with the new package (`apt install ./x.deb`, `dnf install`, `pacman -U`); the config in `/etc` is kept. For a `--install` copy, run the new binary's `--install` (it stops, copies, restarts). The gateway's own `gateway_update` RPC answers 501 (`INSTALL_AND_LOCAL_UI.md:297-300`) | same | the installer again (today) |
| **Update, Docker** | recreate on the new tag with the same volumes, env, devices and flags (today: three env vars only) | same | n/a |
| **Control** | `systemctl start/stop/restart`; a drop-in for env | same | `sc.exe` |
| **Logs** | journal (today), plus the rotating files in the data dir | same | **files** in the data dir (to add, B6) |

### 3.1 Taking over a native install safely

What to find, in order:

1. **Service:**
   - Linux: the unit whose ExecStart is our program, as today
     (`gateway_native.rs:106-129`). Add `/usr/local/lib/sentient-gateway/sentient-gateway` (B5).
     Read its `--config` argument (B4) and its `Environment=` and drop-ins
     (`systemctl show -p Environment,DropInPaths`).
   - Windows: `sc qc SentientGateway` and the registry `Environment`
     (today).
2. **Version:** `program --version`, else the package.
3. **Install shape:**
   - package: `dpkg -S`, `rpm -qf` or `pacman -Qo` (today);
   - the `--install` copy (`/usr/local/lib/...`);
   - by hand. Each shape decides the update path.
4. **Config:** read `st_gateway.json`, or a legacy `tb_gateway.json`, as
   root, via rootjob or the agent (B3).
   - Show the host, port, credential type (never the token), the page
     settings, and the number of connector files and their names.
5. **Python gateways:**
   - a `thingsboard-gateway` unit or a `SentientGatewayService.exe` (the
     gateway's own installer refuses these, `install/windows.rs:56,415`);
   - a Python container.
   - Offered as an update to the Rust gateway (§0.1).

Never lose the connectors:

- Before any take-over action that writes, copy the whole config dir to
  `/var/backups/sentient-gateway-<ts>/` (or `ProgramData\Sentient Gateway backup <ts>`).
  The removal already does this (`gateway_native.rs:239-246`).
- Write only through the gateway's own CLI (`--install`, `--ui-password`),
  never by hand-editing JSON.
- Connectors come back from SENTIENT on the next connection in any case,
  but an offline site boots from the mirror (`mirror.rs:8-11`). So the
  mirror is the asset to protect.

The take-over then records the gateway, as today, **and** marks it
manageable:

- update by the install shape;
- settings by the native file and CLI.

A Docker take-over records the container and offers to "re-create it the
Platform Manager's way":

- same volumes (its own names), same env, devices and network;
- only after showing the diff;
- never for a compose container, whose file stays authoritative. That
  container gets "Open compose folder" plus the exact lines to change.

---

## 4. Proposal: "complete control", phased

### 4.1 The Gateway page

Keep the current status card: running pill, Version, Sends to, Runs as,
Web page, and the actions bottom-right. Add **"Connected to SENTIENT"**
from the local API's `GET /api/status`, or from the platform's device
activity when the page is off. Below it, a settings card built like
Platform settings:

- `ui::settings_card` + `ui::card_tabs` + `ui::setting_row` + the Apply
  bar (`app/ui.rs:2163-2330`, `app/manager/settings.rs:1-21`);
- the `Restart` pill on rows that need a restart;
- an Apply confirmation that names the restart.

| Tab | Rows | Source / apply |
|---|---|---|
| **Connection to SENTIENT** | Host, MQTT port, Credential type (Access token / Username+password / TLS token / X.509), token or user/password/client id, CA / cert / key files, Proxy | not connected or not commissioned: editable, applied via `--install` (native) or a recreate (Docker, moving the token out of the env, see P2); a "Test connection" that uses the local API's test (never the live token). **Connected: locked** (§4.2) |
| **Local web page** | On/off, Listen (loopback only / all interfaces / one address), Port, Extra host names, Password (set or change), Certificate (self-signed / upload) | `localUi.*` via `--install --ui/--no-ui --ui-listen` + a restart; password via `--ui-password -`; the certificate via the local API |
| **Connectors** | List with type, state, device count; an on/off switch per connector | `GET /api/connectors` / `/api/config`. **Locked** while SENTIENT owns them; "Edit here" only after Q3 |
| **Storage** | Type, path, max records / size, TTL | `/api/config` (`storage_configuration`); locked when SENTIENT pushed one |
| **Logs** | Level per logger, remote logging level, log file location | `/api/config` (`logs_configuration`, `RemoteLoggingLevel`); locked like Storage; **plus** the machine-side `RUST_LOG` default (unlocked, restart) |
| **Statistics** | Enable, period, custom commands (read-only list) | `general_configuration.statistics`; locked when pushed |
| **Advanced** | Remote shell (read-only, with a security note), rate limits, max payload, service account, restart policy, serial devices (Docker: `devices` + `group_add`; native: the `dialout`/`uucp` group, `gw/packaging/linux/sysusers-*.conf`), environment overrides (a table like Platform settings › Advanced) | machine-side rows editable (recreate, drop-in or `sc config`); platform rows locked |

How SPM talks to the gateway:

- **Machine-side:** the CLI and the service manager, run as root through
  rootjob, or on a server through the agent. These need no password.
- **Live state and the platform-owned view:** the local API on
  `127.0.0.1:8642`. It needs the page on, and SPM signed in.
  - **Option A:** SPM asks the operator for the page password once per
    session.
  - **Option B (cleaner):** a new gateway CLI, `sentient-gateway --status-json`,
    which reads the files plus a state file, for SPM with root and no
    password. That is a gateway-side change (Q2).
  - With the page off and option B absent, SPM shows the file view only.

### 4.2 Locked settings

- **Lock icon on each row:**
  - The row has a lock icon in place of the field's edit affordance, and
    the value is shown as text.
  - The (i) tooltip gives the reason: "SENTIENT manages this. Change it in
    SENTIENT › Gateways › <name> › Configuration. Saved there at <ts>."
- **Notice at the top of a locked tab:**
  - One muted line with the lock: "SENTIENT manages these settings for this
    gateway."
  - A link to the SENTIENT page, when SPM knows the platform URL from the
    local installation record.
- **Lock rule, per section:**
  - A section is locked once the platform has pushed it (`/api/config`
    shows its `ts`).
  - The connection is locked while the gateway is connected.
  - An unconnected or uncommissioned gateway unlocks the connection.

### 4.3 Install: choose Docker or Native

| OS | Native | Docker |
|---|---|---|
| Debian/Ubuntu (amd64, arm64, armhf) | **default**: the `.deb` from the release, then `--install` | offered if Docker is there |
| Fedora/RHEL | `.rpm` | offered |
| Arch | Arch package | offered |
| Other Linux with systemd | `tar.gz` + `--install` | offered |
| Linux without systemd | **not possible** (the gateway's installer needs systemd) | only Docker |
| Windows | **default**: the setup (today) | **not offered** (Docker Desktop: no serial, no host network) |
| Remote server | **not offered** (today's rule: the token stays on the server) or a token sent over the agent's stdin (Q4) | same |

Checksums:

- The Windows setup has a `.sha256`, which SPM checks (`gateway.rs:376-387`).
- The Linux packages have `SHA256SUMS` (`publish-gateway-to-sentient-release.sh:42`).
  SPM must check them the same way.

Docker install additions:

- a page password;
- the page on/off;
- a serial device picker (`/dev/serial/by-id/*`);
- the token through a file in the config volume instead of the env.

### 4.4 Take over (adopt)

The dialog shows:

- **What was found:** kind, version, program/unit/package or
  container/compose, where the config is, the host, the credential type,
  the connector count, the page state.
- **What will be managed:** update path, controls, settings tabs, logs.
- **What stays as it is:** compose file, volumes, connectors.
- The buttons: "Not now" and "Take over".

Native and Docker, here and on a server. A compose container gets
"Manage here" with update guidance only (§3.1).

### 4.5 Phases

| Phase | Content | Size |
|---|---|---|
| **P0 – fixes** | B1–B6, B11, B15: native page address, root reads, the `/usr/local/lib` path, `--config` files, Windows log files, honest update reasons, the remote version compare | 2–3 days |
| **P1 – read view** | settings tabs, read-only: file view (as root) and, when the page is on, `/api/config` and `/api/connectors`; the locks and notices; "Connected to SENTIENT" | 4–6 days |
| **P2 – machine-side settings** | Local web page tab (on/off, listen, password, certificate), restart policy, `RUST_LOG`, serial devices; Docker recreate with the token moved out of the env (needs a gateway-side "token from file" or uses the config file; Q6); native via `--install`; Apply bar + restart; local and remote (agent jobs `gateway-settings-json`, `gateway-apply <file>`, secrets over stdin) | 1.5–2 weeks |
| **P3 – connection editing** | Connection tab with the not-connected rule and test connection | 3–4 days |
| **P4 – native install and update on Linux** | Install choice per OS (§4.3); package download + SHA256SUMS; update by package manager / `--install`; arm64/armv7 | 1–1.5 weeks |
| **P5 – take over properly** | the §3.1 inspection, the backup, Docker "re-create the Platform Manager's way" | 1 week |
| **P6 – platform-owned edits (optional)** | "Edit here" for storage, logs, statistics, connectors via the local edit path; **blocked on SENTIENT taking up `localConfigurationEdits`** | 1 week SPM + SENTIENT work |

### 4.6 Risks

- **Two authorities.** Any SPM write to a platform-owned section is undone
  by the next push (`mirror.rs:16-17`). Mitigation: locks (§4.2), and
  local edits only through `apply_local`.
- **Secrets.**
  - Today the token sits in the Docker env (B10).
  - Remote jobs must take secrets on stdin, never argv: the
    `SPM_GATEWAY_TOKEN` env is visible in `/proc` to root only, which is
    acceptable.
  - Never show a token back. The gateway's API never renders it in full
    (`INSTALL_AND_LOCAL_UI.md:258-259`).
- **Config formats.**
  - `st_gateway.json` vs legacy `tb_gateway.json`; the `sentient` vs
    `thingsboard` section (`gateway.rs:303`).
  - The `security` variants.
  - Python `logs.json`.
  - Use the gateway's CLI and API; parse only for display.
- **Container rights.** The image runs as uid 65532, so a config volume
  written by root from outside can become unreadable to it. Write through
  `docker exec` as the container's user.
- **Restarts cost data freshness.**
  - Every machine-side Apply restarts the gateway.
  - The Apply confirmation must say so, as Platform settings does.
  - The gateway buffers in SQLite, so no data is lost.
- **Version skew.** `--ui-listen`, `--ui-password` and the local API exist
  from 4.12.0 (`INSTALL_AND_LOCAL_UI.md:185`). For an older gateway, show
  the settings read-only.

---

## 5. Open questions for the owner

1. ~~**Default install on Linux:** native or Docker?~~ **Answered
   2026-11-18:** Native (package + systemd), preselected; Docker as the
   alternative.
2. **How SPM reads live state:**
   - ask for the page password (option A), or
   - add a root-only `--status-json` to the gateway (option B,
     gateway-side work)?
3. **Platform-owned settings in SPM:**
   - read-only for good, or
   - editable through the local edit path?
   The second needs SENTIENT to take up `localConfigurationEdits`, which it
   does not do today.
4. **Remote install:** keep "install on the server itself", or allow SPM to
   install remotely with the token sent over the agent's stdin?
5. **arm64 image:** is `quakestring/sentient-gateway-rs` published
   multi-arch? The Dockerfile has no platform logic, and this was not
   verified.
6. **Docker token:** move it out of the env into the config volume (the
   gateway already prefers the file's token when the env holds a
   placeholder, `config.rs:1159-1172`)? That changes how existing SPM
   containers are recreated.
7. **Windows Docker Desktop gateways:** detect and control only (today),
   or drop them?
8. **`remoteConfiguration: false`** is parsed but ignored by the gateway.
   Should it lock SENTIENT out (a gateway-side change), so that an
   SPM-managed site could choose local authority?
