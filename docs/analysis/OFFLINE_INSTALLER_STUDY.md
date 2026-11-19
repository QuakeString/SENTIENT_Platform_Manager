# SPM offline installer: design study (Windows x64)

Status: study only, no code changed. Branch `spm/0.9.3`, written 2026-11-19.
Scope (owner, 2026-11-19): **Windows x64 only for now.** Linux gets a short "Later" note at the end.

Goal: one file per release, `SPM-offline-installer-<spm>-SENTIENT-<ver>-windows-x64.zip`
(+ `.sha256`), with a signed manifest inside. It installs SENTIENT (native or Docker-in-WSL2)
with no network, and a newer pack updates an installation the same way (for example from a USB stick).
It ships as its own release after 0.9.3.

---

## 1. What exists today

| Piece | Where | What it does | Gap for a true offline pack |
|---|---|---|---|
| `ArtifactSource::{Bundled, Remote}` | `crates/sentient-installer-core/src/native.rs:62` | The native engine's offline/online split. `obtain()` (:165) copies a file from the bundle dir or downloads it. | Native only. WSL, Docker, images and the gateway never look at it. |
| `required_bundle_files()` | `native.rs:2384` | Windows: PG zip, TimescaleDB zip, `sentient-payload.tar`. vc_redist is optional. | No gateway, no WSL or Docker items, no versions, no hashes. |
| `bundled_payload_dir()` | `native.rs:2404` | Finds `payload\` next to the running exe when all required files are present. | Checks only that the files exist. No manifest, signature or SHA-256. |
| `verify_bundle()` | `native.rs:2461` | Preflight: reports missing files. | It checks presence only. |
| `stage_payload()` | `native.rs:2427` | Unpacks `sentient-payload.tar` with `tar.exe`. It is the only path that installs SENTIENT itself on Windows. | OK as is. |
| `ensure_vc_runtime()` | `native.rs:540` | Uses vc_redist from the bundle, or else from `aka.ms`. | OK. The pack always carries it. |
| Setup bundle detection | `app/src/installer/mod.rs:324`, `plan.rs:168` | `bundle = bundled_payload_dir()` → `ArtifactSource::Bundled`. Reads `payload\VERSION`. | It never sees a pack in a folder the user chose. |
| Online package download | `plan.rs:538` → `app/src/online.rs:121` | Fetches the signed `manifest.json`, then `download_package()` writes the same folder layout plus `VERSION`. | Network only. Its layout is the model for the pack. |
| "Install from local files" | `app/src/ui.rs:609`, `app/src/standalone.rs:31` | The user picks an unpacked folder that holds `SENTIENT-Setup.exe` + `payload`. That Setup starts with `--install --add-sentient`. | No checks at all; it trusts the folder. |
| "Update from a file" | `app/src/manager/update.rs:765`, `app/src/updating.rs:31` | A file named `*setup*` gets run. A folder is treated as a native package: `staged()` unpacks its payload tar. | No signature, no SHA-256. The **Docker engine ignores the package** and pulls from Docker Hub. The gateway is not covered. |
| `SPM_DEBUG_PACKAGE_DIR` | `app/src/maintenance.rs:115` | Debug builds only: fakes a package found next to the program (with `SPM_DEBUG_PACKAGE_VERSION`). | It only tests the UI. |
| Release manifest | `crates/sentient-installer-core/src/manifest.rs` | `Manifest {product, version, released, notes, build_version, min_glibc, artifacts[file,url,sha256,size,kind,into]}`. `fetch_signed()` (:121) checks `manifest.json.minisig`. | URL-only (`fetch_body`). There is no "verify from a file". |
| minisign check | `crates/sentient-installer-core/src/signing.rs` | ed25519 key `RELEASE_PUBLIC_KEY` (id 0A8C8F7EFCE2CBA2). `check()` warns on unsigned files until signing has started. | A pack must **refuse** unsigned: there is no feed to say whether signing has started. |
| Offline Setup package | `scripts/package-windows.ps1` | Builds `SENTIENT-Setup-<v>\{SENTIENT-Setup.exe, EULA.txt, payload\{PG zip, TSDB zip, sentient-payload.tar, vc_redist, VERSION, NOTES.md}}` and zips it. | This is the native half of the pack already. It has no manifest, no gateway and nothing for WSL or Docker. |

Release folder `/home/mz/INVENIA/sentient-release-build/4.4.0.3-release/`:

| File | Size |
|---|---|
| manifest.json / manifest.json.minisig | 2.3 KB / 277 B |
| sentient-payload.tar (Windows) | 515 MB (491 MiB) |
| sentient-payload-linux-x86_64.tar | 529 MB |
| postgresql-18.6-1-windows-x64-binaries.zip | 344 MB |
| timescaledb-postgresql-18-windows-amd64.zip | 8.4 MB |
| vc_redist.x64.exe | 25.6 MB |

## 2. What a Windows install downloads today

| Engine / flavour | Item | Source today (code) | Approx size |
|---|---|---|---|
| all | SENTIENT Setup / SPM exe | GitHub `sentient-release` (`spm-*` tags) | ~148 MB (0.9.2-rc1 exe) |
| native | SENTIENT payload | `manifest.json` → GitHub release asset (`online.rs`) | 515 MB |
| native | PostgreSQL 18.6 binaries | release asset (EDB mirror `get.enterprisedb.com`, `native.rs:43`) | 344 MB |
| native | TimescaleDB 2.29.2 | release asset (GitHub timescale, `native.rs:47`) | 8.4 MB |
| native | VC++ runtime | release asset, else `aka.ms/vs/17/release/vc_redist.x64.exe` (`vcrt.rs`) | 26 MB |
| native / docker | Sentient Gateway (Windows service) | `SentientGatewaySetup-<v>.exe` + `.sha256` from `gateway-<v>` releases (`gateway.rs:358, :613`) | 11 MB |
| docker, native+ClickHouse | WSL features | `wsl --install --no-distribution`, falls back to DISM (`wsl.rs:65`) | 0 (from the component store), plus a **reboot** |
| docker, native+ClickHouse | WSL package | `github.com/microsoft/WSL/releases` latest MSI (`wsl.rs:263, :282`) | ~200-250 MB (verify) |
| docker, native+ClickHouse | WSL 2 kernel (Windows 10 before KB5020030 .2311) | `wslstorestorage.blob.core.windows.net/.../wsl_update_x64.msi` (`wsl.rs:196`) | ~17 MB |
| docker, native+ClickHouse | Ubuntu 24.04 rootfs | `cloud-images.ubuntu.com/wsl/releases/24.04/current/...rootfs.tar.gz` (`distro.rs:38, :1266`) | ~350 MB |
| docker, native+ClickHouse | Docker Engine in the distro | `curl -fsSL https://get.docker.com \| sh` (`distro.rs:1310`) → apt from download.docker.com and archive.ubuntu.com | ~120 MB of .debs |
| docker | quakestring/sentient:<v> | `docker compose pull` (`distro.rs:917, :2140`) | 1.21 GB unpacked, ~0.45 GB zstd |
| docker (timescale) | timescale/timescaledb:2.26.1-pg18 (`distro.rs:44`) | Docker Hub | ~1.4 GB unpacked, ~0.5 GB zstd |
| docker (stock) | postgres:18-alpine | Docker Hub | 433 MB, ~0.15 GB zstd |
| docker hybrid, native+ClickHouse | clickhouse/clickhouse-server:24.8-alpine | Docker Hub (in the `sentient-clickhouse` account's distro, `clickhouse_wsl.rs`, `agent.rs:36`) | 677 MB, ~0.25 GB zstd |
| docker gateway | quakestring/sentient-gateway-rs:<v> | Docker Hub | 244 MB, ~0.09 GB zstd |
| kiosk (optional) | Chrome enterprise MSI | `dl.google.com` (`kiosk.rs:109`). Used only when Edge is missing. | ~120 MB. Do not ship it; Edge is in Windows. |
| background | release lists, Docker Hub tags, licence | `api.github.com`, `hub.docker.com` (`releases.rs`, `dockerhub.rs`, `fetch_cache`) | Metadata only. Must fail quietly when offline. |

Sizes of the images are local `docker images` sizes. The zstd figures are estimates (the ratio is usually 35-40 %).
Note: the **native ClickHouse flavour also needs the whole WSL + Docker chain**, because ClickHouse runs in Docker
inside a hidden account's WSL distro.

## 3. Pack layout

```
SPM-offline-installer-0.9.4-SENTIENT-4.4.0.4-windows-x64/
  SENTIENT-Setup.exe              SPM (signed Authenticode); finds pack.json beside it
  EULA.txt, README.txt            README: "double-click SENTIENT-Setup.exe"
  pack.json                       the pack manifest (below)
  pack.json.minisig               minisign, release key
  payload/                        unchanged native layout (package-windows.ps1)
    sentient-payload.tar  postgresql-18.6-1-windows-x64-binaries.zip
    timescaledb-postgresql-18-windows-amd64.zip  vc_redist.x64.exe  VERSION  NOTES.md
  gateway/SentientGatewaySetup-4.16.2.exe
  wsl/
    wsl.2.6.x.0.x64.msi           WSL package (Windows 11 / 10 22H2 .2311+)
    wsl_update_x64.msi            kernel for older Windows 10
    ubuntu-noble-wsl-amd64-<date>.rootfs.tar.gz   pinned by date, not "current"
    docker-debs/*.deb             docker-ce, -cli, containerd.io, buildx, compose plugin (+ deps)
  images/                         docker save, one file per image, zstd
    sentient-4.4.0.4.tar.zst  timescaledb-2.26.1-pg18.tar.zst  postgres-18-alpine.tar.zst
    clickhouse-24.8-alpine.tar.zst  sentient-gateway-rs-4.16.2.tar.zst
```

`pack.json` (a superset of `Manifest`, so `manifest.rs` can share types):

| Field | Example / meaning |
|---|---|
| `format` | `1`. SPM refuses a newer format with a clear message. |
| `target` | `windows-x64` |
| `min_os` | `{ "windows_build": 19044 }`; native min to be confirmed. Docker needs 19041+. |
| `spm_version`, `sentient_version`, `gateway_version` | `0.9.4`, `4.4.0.4`, `4.16.2` |
| `components` | `{ "postgresql": "18.6-1", "timescaledb": "2.29.2", "wsl": "2.6.x", "ubuntu_rootfs": "24.04-<date>", "docker": "<ver>", "images": {...digests} }` |
| `files[]` | `{ path, sha256, size, kind (payload/pg/tsdb/vcredist/gateway/wsl_msi/kernel_msi/rootfs/deb/image), image_ref?, image_digest? }` |
| `min_glibc` | omitted on Windows (Linux-only field, kept for later targets) |
| `created`, `notes` | build date; release notes for "What's new" |

The signature covers `pack.json` only. Each file is covered by its SHA-256 in it. The outside `.sha256` is for the person
copying the zip, so they can check it before unpacking.

Sizes (approximate):

| Part | On disk |
|---|---|
| SPM exe + EULA | ~0.15 GB |
| payload/ (SENTIENT + PG + TSDB + vc_redist) | ~0.89 GB |
| gateway/ | 0.01 GB |
| **Native pack (PostgreSQL / TimescaleDB flavours)** | **~1.05 GB raw, ~0.8 GB zipped** |
| wsl/ (msi ~0.25 + kernel + rootfs ~0.35 + debs ~0.12) | ~0.74 GB |
| images/ (5 images, zstd) | ~1.45 GB (~4.0 GB once loaded) |
| **Full pack (adds Docker-in-WSL and native+ClickHouse)** | **~3.2 GB**: over GitHub's 2 GiB per-file limit, under FAT32's 4 GiB |

Recommendation: publish **two files per release**:
`...-windows-x64.zip` (the native pack, everything except images and WSL) and
`...-windows-x64-docker.zip` (wsl/ + images/, ~2.2 GB; split by image if needed to stay under 2 GiB).
A USB copy holds both, unpacked into one folder, and Setup treats them as one pack. If the owner insists on one file,
host it outside GitHub (`manifest.rs:84` already resolves Google Drive links) or ship `.zip.001/.002` parts.

## 4. How Setup uses a pack

Order (nothing is changed on the computer before step 4):

1. **Find it**: `pack.json` next to `SENTIENT-Setup.exe`, or in a folder or `.zip` the user picks ("Install from an offline pack").
   A zip is unpacked to `%ProgramData%\SENTIENT\pack-<ver>` first. Only `pack.json` is read in place.
2. **Verify**: minisign over `pack.json` with `RELEASE_PUBLIC_KEY`. An unsigned or bad signature is refused; packs have no
   "before signing" grace. Then the SHA-256 and size of every listed file, with a progress bar (~3 GB takes about 10-30 s).
   Any mismatch → "The pack is damaged: <file>. Copy it again." Extra files are ignored. Missing optional parts
   (no `-docker` half) only grey out those choices.
3. **Target check**: `target == windows-x64`, the processor is x86_64, and the Windows build is at least `min_os`.
   A mismatch gets a plain reason, for example "This pack is for Ubuntu 24.04; this computer runs Windows 11. Download the
   windows-x64 pack." or "This pack is for 64-bit Intel/AMD Windows; this computer has an ARM processor."
   It also checks free disk space against the sum of the sizes ×2.5.
4. **Install**, with every downloader pointed at the pack:

| Code point | Change |
|---|---|
| `native.rs:2384/2404/2461` | `required_bundle_files` / `bundled_payload_dir` / `verify_bundle` read `pack.json` (still accepting today's `payload\` folders, with a visible "unsigned package" warning) |
| `manifest.rs` | new `Pack` type, `Pack::open(dir)`, `verify_signed_file()`. `signing::check` gets a strict mode. |
| `app/src/installer/mod.rs:324`, `plan.rs:168, :538` | `bundle` becomes `Option<Pack>`. Never call `online::download` when a pack is present. |
| `app/src/standalone.rs:31`, `ui.rs:601-609` | "Install from local files" accepts a pack folder or zip and verifies it before starting Setup |
| `wsl.rs:65` `install()` | With a pack: DISM for the two features (offline), then `msiexec /i wsl/<msi>`. Skip `wsl --install` (it downloads on new builds) and `latest_package_url()`. `install_signed_msi` (:289) takes a local path; the Authenticode check stays. |
| `wsl.rs:166` `ensure_kernel` | local `wsl_update_x64.msi`; no `wsl --update` (it goes to the Store/GitHub) |
| `distro.rs:1264-1266` | rootfs from `wsl/` instead of `download(ROOTFS_URL)` |
| `distro.rs:1310` | `dpkg -i /mnt/c/.../docker-debs/*.deb` instead of `get.docker.com`. Keep `IPTABLES_LEGACY_IF_NEEDED`. |
| `distro.rs:917, :2140` (`compose pull`) | `docker load -i` each image (zstd: SPM decompresses it, or `docker load` reads zstd on Engine 23+), then `up --pull never` |
| `agent.rs:36`, `clickhouse_wsl.rs` | the same rootfs/debs/image path inside the `sentient-clickhouse` account. The pack copy must sit where that account can read it (`%ProgramData%\SENTIENT\pack-*`, not the user's Downloads). |
| `gateway.rs:613` `install()` | the installer from `gateway/`, SHA-256 from `pack.json` (not `fetch_text`). The gateway offer comes from the pack, not `releases::list`. |
| `dockerhub.rs`, `releases.rs`, `updates.rs` | No network: the pack's versions are "newest known". No error banners, a quiet "offline" label. |
| `kiosk.rs:220` | Do not fetch Chrome offline. Use Edge, or say "no browser for the kiosk". |

## 5. Updating from a pack

Today "Update from a file" (`manager/update.rs:765`) runs a `*setup*` exe, or for a folder sets `page.package`, and
`updating::run_to` uses it only for **native** (`staged()`, `updating.rs:31`). It runs without any signature or hash check.
A Docker installation still pulls from Docker Hub, the gateway is not touched, and the Platform Manager updates only when the user
picks the Setup exe by hand.

Proposal: rename the action to **"Update from an offline pack"** (it still accepts a lone Setup exe).

1. Pick a folder, a zip or the USB drive root. Verify it as in section 4 (signature, hashes, target).
2. Show a table: installed vs pack for the Platform Manager, SENTIENT, the gateway, and PostgreSQL/TimescaleDB (database engines are
   shown but not upgraded by a pack in phase 1; a PG major upgrade stays its own flow). Downgrades are refused unless confirmed
   (`releases::movement`).
3. **SPM first**, if newer: copy the pack to `%ProgramData%\SENTIENT\pack-<ver>`, run the new `SENTIENT-Setup.exe --update-from-pack <dir>`,
   close. The new SPM resumes with steps 4-5 so a newer SPM drives a newer SENTIENT.
4. **SENTIENT**: native → the existing `staged()` + `native_linux/native::update` with the pack's payload. Docker → `docker load` of the
   SENTIENT image, then `docker_ops::safe_update_to` with no pull (the snapshot + rollback stay as they are).
5. **Gateway**: Windows service → `gateway::install(offer from pack, None)`. Docker gateway → `docker load` + recreate.
6. Keep the last pack folder for rollback (`rollback_point`) and delete older ones.

## 6. Release pipeline

| Step | Where | Notes |
|---|---|---|
| SENTIENT release folder | st-dev, `sentient-release-build/<v>-release/` (today) | already signed `manifest.json` |
| SPM Windows exe | PR CI `build-installers.yml` (windows-latest), Authenticode via `sign-windows.ps1` | same exe as the online release |
| Gateway exe | `gateway-<v>` release on `sentient-release` | download + check its `.sha256` |
| WSL MSI + kernel MSI | Microsoft (GitHub WSL release, pinned tag), checked for Authenticode | pin the version per pack |
| Ubuntu rootfs | `cloud-images.ubuntu.com/wsl/releases/24.04/<date>/`, pinned | also pin `ROOTFS_URL` online for parity |
| Docker .debs | `docker run ubuntu:24.04` with Docker's apt repo, `apt-get download` + `apt-cache depends --recurse` against the rootfs's own package list | the exact versions go in `pack.json` |
| Images | `docker pull` by digest, `docker save \| zstd -19` | amd64 only |
| Assemble + sign | new `scripts/package-offline-windows.sh` on st-dev (Linux): lay out the tree, write `pack.json`, `sign-release.sh` (minisign key lives there), zip, `.sha256` | No Windows machine needed. Build in `/mnt/data_1`; /tmp is RAM. |
| Publish | `scripts/publish-spm-to-sentient-release.sh`-style, release `offline-<spm>-<sentient>` on `QuakeString/sentient-release` | ~0.8 GB + ~2.2 GB per release. GitHub: **each asset < 2 GiB**, no total cap. Split the docker half by image if needed. |
| Smoke test | air-gapped Win10 VM (`spm-win10-test`, NIC removed) + Win11 | native, docker, native+ClickHouse, update |

## 7. Risks and open questions

| # | Risk | Detail | Mitigation / question |
|---|---|---|---|
| 1 | **Licence needs the internet** | On origin/master a fresh install has no licence, and `NO_LICENSE_LIMITS` blocks all creation (`license_enforcer.rs:243`). Activation is `activate_via_license_server()` (`license_service.rs:331`). `import_license_file()` (:1151) only extracts the key and then calls the server ("Offline licenses are not supported"). The demo licence also comes from the server. Since dfff3753 (on master), **VM/WSL deployments must reach the licence server within 90 days** or they restrict themselves. Docker-in-WSL counts as virtualised. | The owner must decide: an offline activation (signed request/response file carried by USB, server-side support), or "pack installs offline, activation needs one connection, from any machine". It blocks a truly offline site. |
| 2 | **WSL needs Windows' help** | The features turn on offline through DISM from the component store, but a **reboot** is required. BIOS virtualisation must be on, and SPM cannot fix that. `wsl --install` / `--update` download, so the pack must carry the MSI. Windows 10 before .2311 can't run the WSL package and needs the kernel MSI. Some LTSC/locked images have the features removed, and DISM then wants a source/Windows Update. | Carry both MSIs; use the existing reboot-and-resume. Detect "feature payload removed" and say so (a `/Source:` install media hint). |
| 3 | **Size / transport** | The full pack is ~3.2 GB: over GitHub's 2 GiB per asset and close to FAT32's 4 GiB per file once images grow. | Two-file split (native / docker), one file per image; tell users to format sticks as exFAT/NTFS. |
| 4 | Docker .debs drift | The rootfs and the .debs must match one Ubuntu point release. A missing dependency (iptables, libltdl7, pigz…) only shows up offline. | Resolve deps against the pinned rootfs in CI; test with the network off. |
| 5 | Version skew | TimescaleDB is 2.29.2 native vs 2.26.1 in the Docker image. Packs freeze both. | Record both in `pack.json`; align them in the next release. |
| 6 | Time sync | Signature and SHA checks don't depend on the clock. Licence time is on its tamper-resistant clock. A badly wrong clock can still upset TLS later and log timestamps. | A pre-install check: warn if the clock is >1 day off from the pack's `created` (warn only, never refuse). |
| 7 | Background network calls | Release lists, Docker Hub tag checks and the licence heartbeat will retry and log errors on an air-gapped site. | An "offline site" mode in SPM settings that stops update checks. |
| 8 | Unpacking cost | Unpacking 3 GB, hashing, plus `docker load` (~4 GB written into the vhdx) takes 10-20 min on slow disks. | Show it in the plan weights; hash once. |
| 9 | Open | Minimum Windows build for native? Do we keep the unsigned `payload\` folder path? Who builds the pack (a CI job or st-dev by hand)? | owner |

## 8. Phased plan

| Phase | Work | Effort |
|---|---|---|
| P0 | Decisions: licence offline story (risk 1), one file vs two, pack naming/release tag | 0.5 d + owner |
| P1 | `Pack` type, `pack.json` + minisign strict verify, all-files SHA-256 with progress, target/OS/disk check, wrong-OS messages; unit tests | 3 d |
| P2 | Native offline end to end: Setup + "Install from an offline pack" use the pack; gateway from `gateway/`; quiet offline release checks | 2-3 d |
| P3 | Docker-in-WSL offline: local WSL MSI/kernel, pinned rootfs, local .debs, `docker load` + `--pull never`; the same for the ClickHouse account | 5-7 d |
| P4 | "Update from an offline pack": verify, versions table, SPM-first hand-off, SENTIENT native + Docker, gateway, rollback | 3-4 d |
| P5 | `scripts/package-offline-windows.sh`, signing, split, publish to `sentient-release`, README | 2-3 d |
| P6 | Air-gapped tests: Win10 VM (NIC removed) and Win11; native, timescale, hybrid, Docker; reboot-resume; update from USB | 2-3 d |
| | **Total** | **~3-4 weeks** |

## Later: Linux targets (one line each)

All of them use the one glibc 2.34 Linux payload from the next SENTIENT release, and each pack fits only its exact distro release.

- **Ubuntu 24.04 / 22.04**: a local `file:` apt repo of `apt-get download`ed .debs (PostgreSQL, TimescaleDB from packagecloud), pinned versions.
- **Debian 12 / 13**: the same apt approach; TimescaleDB needs its own repo's .debs per release, so check they exist for 13.
- **Rocky / Alma 9**: a local dnf repo (`createrepo_c`); TimescaleDB rpm from Timescale's repo; PG from PGDG.
- **Arch**: a local pacman repo (`repo-add`). TimescaleDB is in the official repos, but a rolling release breaks pinning, so the pack goes stale fast.
- **ClickHouse (all Linux)**: already the vendor static tgz (`native_linux.rs:309`), so ship that file; Arch has no package.
- **Docker engine on Linux**: not shipped in phase 1. Native is the offline path; Docker needs an engine installed already.
- **arm64**: later; doubles every pack.
