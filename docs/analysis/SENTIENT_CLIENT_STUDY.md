# SENTIENT Client: distribution and install through sentient-release and SPM

Study, 2026-11-19. Read-only research, nothing built or changed. Mockup: `spm-client-mockup.html` (Setup step,
Summary, Platform settings, Update, kiosk frame).

**Scope (owner, 2026-11-19): the Client is Windows only (WebView2).** On Linux, SPM keeps opening SENTIENT and the
kiosk in a browser (Chromium / Chrome / Brave / Edge, then Firefox), as it does today. No deb/rpm/AppImage is published
or installed. Section 9 lists the WebKitGTK problems behind that call.

## 1. Summary

- The Client is `tauri-app/` (Tauri 2.10.3, WebView2 on Windows), v0.1.0, `com.invenia.sentient`. Today it **embeds
  the whole Angular UI** and asks for the server address in a dialog. It has no kiosk mode, no single instance, no
  command-line arguments, no signing and no updater.
- Recommendation: make it a **thin shell that loads the SENTIENT URL** it is given, instead of a copy of the UI.
  The UI then always matches the server, extensions load as they do in a browser, and the installer is a few MB.
- Publish it as **its own release `client-<ver>`** on `QuakeString/sentient-release`: a pre-release, never Latest,
  with its own signed `manifest.json` (SHA-256 per file, minisign), Authenticode-signed MSI and NSIS, and a row in
  the README Downloads table.
- SPM installs the **MSI** (per machine, silent, clean uninstall and upgrade), and first the **WebView2 Evergreen
  runtime** if it is missing: the bootstrapper online, the Standalone installer from the offline pack.
- Shortcut, sign-in entry and kiosk open the Client; Edge, then Chrome, then Firefox are the fallback.
- The withdrawn crash (2026-11-06) was a **different app** on **Linux/WebKitGTK**. Its cause is unknown. The Windows
  Client has never had a stability test: one is phase 0 of this plan.
- Effort: about **20-24 working days**, plus lead time for a code-signing certificate.

## 2. What the Client has today

| Item | State on master | Gap |
|---|---|---|
| Engine | Tauri 2.10.3, wry 0.54.2, WebView2 (webview2-com 0.38.2) | none |
| Content | `frontendDist: ../ui-ngx/target/generated-resources/public` (the UI build, 118 MB folder with its pre-compressed copies) | UI version is frozen into the Client; server extensions (`/api/resource/js_module/...`) and CORS from the `tauri://` origin are untested |
| Server address | first-run dialog (`core/tauri/backend-config*`), kept in WebView localStorage `sentient.tauri.backendUrl` | **cannot be passed as an argument**; SPM cannot set it |
| Window | 1400x900, resizable, `fullscreen: false`, title "SENTIENT IoT" | **no kiosk** (full screen, no close, no escape) |
| Instances | none | **no single instance**: each shortcut click opens one more window |
| Plugins | `tauri-plugin-shell` (`shell:allow-open`), `withGlobalTauri: true` | shell is not needed in a shell that loads a remote page; global IPC must be off for remote content |
| Arguments | none (`lib.rs` is the default builder) | `--url`, `--kiosk`, `--version` |
| Starting page | none | SPM's `starting.html` waits for SENTIENT after a reboot; the Client needs the same |
| Crash recovery | none | WebView2 "process failed" -> reload; a kiosk must never show a dead page |
| Bundle | `targets: all`; CI builds msi+nsis, deb/rpm/AppImage, dmg | Windows only now; product name "SENTIENT IoT", not "SENTIENT Client" |
| Signing | none. CI passes `TAURI_SIGNING_PRIVATE_KEY`, but that is the **updater** key and no updater is configured | **Authenticode** for the exe, MSI and NSIS |
| Updater | none | keep it that way: SPM is the one updater (decision D6) |
| Version | 0.1.0 in two files; tags `desktop-v0.1.0`, `desktop-v0.1.1` (only a draft GitHub release, never in sentient-release) | one version source, set from the tag |
| Linux | `main.rs` sets `GTK_IM_MODULE`, forces GPU compositing, re-enables DMABuf "for testing" | dropped from the plan (section 9) |

## 3. Prerequisites in the Client (to build before SPM ships it)

| # | What | How (Tauri 2) | Effort |
|---|---|---|---|
| C1 | Load the given URL, not a bundled UI | window URL `WebviewUrl::External`; a local starting page polls `<url>/api/noauth/...` and navigates when it answers; `frontendDist` becomes that one page | 1.5 d |
| C2 | Arguments | `SENTIENT-Client.exe --url http://localhost:8080 [--kiosk]`; without `--url`, `%ProgramData%\SENTIENT\client.json` written by SPM, else the existing first-run dialog | 0.5 d |
| C3 | Single instance | `tauri-plugin-single-instance`: a second start focuses the window (and passes its URL); kiosk and window cannot run side by side | 0.5 d |
| C4 | Kiosk switch | `--kiosk`: fullscreen, no decorations, always on top; `CloseRequested` prevented; F11/Esc/Alt+F4/Ctrl+W/Ctrl+R/zoom/context menu/devtools off; navigation locked to the SENTIENT origin; `window.open` and external links refused | 2 d |
| C5 | Admin exit from the kiosk | a key chord, then a password set in SPM (D4) | 1 d |
| C6 | Crash recovery | WebView2 `ProcessFailed` -> reload the starting page; needs `with_webview` + webview2-com (the crate is `deny(unsafe_code)`, so one scoped `allow`) | 1 d |
| C7 | Hardening | `withGlobalTauri: false`, drop the shell plugin, no IPC capability for the remote origin, CSP on the starting page | 0.5 d |
| C8 | Signed Windows installer | section 5 | in P2 |
| C9 | Stability test | section 7 | in P0/P5 |

What the app cannot do by itself: block Ctrl+Alt+Del, Win+L, Alt+Tab or the Win key. "Cannot be escaped" at the
OS level needs Windows Assigned Access / Shell Launcher (edition-dependent: to be checked per target edition) or a
keyboard-filter policy. The Client kiosk is as strong as Edge's `--kiosk` today, plus no close and no navigation away.

## 4. Release layout on sentient-release

Recommendation: **own release and own manifest**, not artifacts added to the SENTIENT manifest.

| | Own `client-<ver>` release (recommended) | Inside each SENTIENT release |
|---|---|---|
| Cadence | Client ships when it changes | re-published with every SENTIENT build |
| Immutability | SENTIENT releases stay untouched | needs `--add`, which rewrites a signed manifest |
| SPM lookup | like `spm-v*` releases (`releases.rs` already skips non-numeric tags) | already read |
| Compatibility | `min_sentient` field in its manifest | implicit |

Release `client-0.2.0`: pre-release, `make_latest: false` (Setup reads `releases/latest`, which must stay SENTIENT's).

| Asset | Note |
|---|---|
| `SENTIENT-Client_0.2.0_x64_en-US.msi` | what SPM installs; Authenticode-signed |
| `SENTIENT-Client_0.2.0_x64-setup.exe` | NSIS, for a manual install; signed |
| `manifest.json` + `manifest.json.minisig` | `{"product":"SENTIENT Client","version","released","notes","min_sentient","webview2_min","artifacts":[{file,url,sha256,size,kind:"msi"/"nsis"}]}`, signed with the release key (`sign-release.sh`) |

Changes:
- **CI `tauri-build.yml`**: Windows job only (drop Linux and macOS; drop `build-frontend` once C1 lands); trigger on
  `client-v*` (D7) with the version taken from the tag into `tauri.conf.json`/`Cargo.toml`; Authenticode through
  `bundle.windows.signCommand` calling `sign-windows.ps1` (secrets `SPM_SIGN_PFX_BASE64` / `_PASSWORD`, the same as SPM's
  `build-installers.yml`), so the exe is signed before it goes into the MSI; check FileVersion and signature after the build
  (the Windows stale-binary lesson); write `.sha256`; upload to a **draft** release on QuakeString/SENTIENT. Remove
  `TAURI_SIGNING_PRIVATE_KEY`.
- **New `scripts/publish-sentient-client.sh <ver>`** in SPM (beside the other publish scripts, modelled on
  `publish-spm-to-sentient-release.sh`): fetch the draft's assets, check the Authenticode signature, write and sign
  `manifest.json`, create `client-<ver>` as a pre-release that is not Latest, upload the manifest last, refuse a
  version already published, then run `update-sentient-release-readme.sh`, which gets a "SENTIENT Client (Windows)" row
  (`newest("client-")`). `publish-sentient-release.sh` itself is unchanged.

## 5. WebView2 runtime

Windows 11 has it built in. Most Windows 10 PCs got it through Microsoft's rollout. LTSC, offline and never-updated
PCs may not have it. Detection: registry `...\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` value `pv`
(HKLM WOW6432Node, then HKCU).

| Option | Size (x64, approx.; measure at pack build) | Updates | Fit |
|---|---|---|---|
| Evergreen bootstrapper `MicrosoftEdgeWebview2Setup.exe` | ~2 MB, downloads the rest | automatic (Edge Update) | **online installs** |
| Evergreen Standalone `MicrosoftEdgeWebView2RuntimeInstallerX64.exe` | ~130-190 MB (Tauri's docs say ~127 MB) | automatic once installed, if Edge Update can reach Microsoft | **offline pack (recommended)** |
| Fixed Version (`.cab`, shipped inside the app) | ~180 MB packed, several hundred MB unpacked | **none**: we must ship every security fix | not recommended |

Licensing: redistributing the WebView2 Runtime with an application is allowed by Microsoft's WebView2 distribution
terms at no fee (re-read the current text before the first pack ships). With Fixed Version, keeping that browser
engine patched is our job; on operator screens that face the plant network, that is the deciding reason against it.
Air-gapped Evergreen installs do not update by themselves: a newer offline pack brings a newer Standalone installer,
and SPM installs it when its version is higher.

The MSI keeps Tauri's default `downloadBootstrapper` mode for people who install it by hand. SPM always makes sure
the runtime is there first, so the MSI check finds it and does nothing.

## 6. SPM changes

| Area | Change | Files |
|---|---|---|
| Release lookup | `client_releases()` beside the manager's `MANAGER_TAG_PREFIX` logic; verify the minisign over the manifest, then SHA-256 of the MSI | `sentient-installer-core/src/releases.rs`, `manifest.rs`, `signing.rs` |
| Install core | new `client.rs`: `installed()` (Uninstall key by UpgradeCode), `ensure_webview2(online/pack)`, `install(msi)` = `msiexec /i ... /qn /norestart` (3010 = success, re-detect as `kiosk.rs` does for Chrome), `uninstall()` = `msiexec /x {ProductCode} /qn`, write `client.json` | new `client.rs`, `pack.rs` |
| Offline pack | `client/SENTIENT-Client_<v>_x64_en-US.msi` (~5 MB est.) + `client/MicrosoftEdgeWebView2RuntimeInstallerX64.exe`; new kinds `client_msi`, `webview2`; `pack.json` gets `client_version`. Adds ~0.14-0.2 GB to the ~1.05 GB native pack. The Chrome MSI stays out (Edge is the fallback) | `pack.rs`, `package-offline-windows.sh` |
| Setup, Welcome | third choice, Windows only: **"Only the SENTIENT Client (operator PC)"**, next to "Only the Platform Manager" | `installer/pages.rs`, `mod.rs` |
| Setup, Shortcuts and startup | on Windows: "Open SENTIENT with: (o) the SENTIENT Client (recommended) ( ) a web browser". The kiosk and at-sign-in options then use that. Linux shows no choice (browser) | `installer/pages.rs`, `plan.rs` (`Shortcuts.open_with`) |
| Setup, Summary | row "Opens with": "SENTIENT Client 0.2.0 (installs WebView2 runtime)" or "Microsoft Edge" | `installer/pages.rs` |
| Launch | `--open-sentient [--kiosk]` starts the Client with `--url` (and `--kiosk`) when it is installed and chosen; else today's browser path. Shortcuts keep pointing at SPM, so a removed Client falls back cleanly | `open_client.rs`, `kiosk.rs` |
| Platform settings | Startup tab: "Open SENTIENT with: Client / Browser" next to "Open SENTIENT at sign-in: Off / Kiosk / Window"; Platform tab: a **SENTIENT Client card** (version, WebView2 version, Update, Remove, or Install) | `manager/settings.rs`, `startup.rs` |
| Update page | a "SENTIENT Client" row right after SENTIENT, before the gateway (order: Platform Manager, SENTIENT, SENTIENT Client, Sentient Gateway), from `client-*` releases or the pack | `manager/update.rs` (`product_row`) |
| Record | `InstallationRecord.client: Option<{version, product_code, url, open_with}>` | `installation.rs` |

**Client-only install (operator PC).** Steps: Welcome -> SENTIENT address (a remote URL, checked to answer as
SENTIENT; a self-signed HTTPS certificate is shown and must be accepted) -> Shortcuts and startup -> Summary -> Install
(WebView2, Client MSI, the Platform Manager in its "manager only" form for updates and removal, shortcuts) -> Finish.
No server, database or ports. The starting page waits for the remote server just as it waits for a local one.

## 7. Stability test (before anything is offered to the owner)

| Test | Pass |
|---|---|
| Open the heaviest real dashboards (saf rmhs, furnace, 33kv) on 3 GPUs: Intel UHD, NVIDIA, AMD; Windows 10 22H2 and 11 24H2 | opens; no hang; CPU and memory within 10 % of Edge on the same PC |
| 72 h soak in kiosk mode, live data | no growth beyond +20 % memory; no white page; WebView2 process count stable |
| Kill `msedgewebview2.exe` by hand | Client reloads within 10 s |
| Reboot with SENTIENT starting late | starting page, then SENTIENT, never an error page |
| Every escape key (section 3) | none leaves the kiosk; the admin chord does |
| Second click on the shortcut | focuses the one window |
| Install, update, remove, offline pack, no WebView2 present (Windows 10 LTSC VM) | all silent and clean |

Run first on the `spm-win10-test` VM, then on **the owner's own hardware**, with him confirming (the lesson from
2026-11-06: lab numbers did not match his experience).

## 8. The earlier crash

What crashed on 2026-11-06 was **not `tauri-app`**: it was `sentient-runtime-client` (`/mnt/data_1/INVENIA/
sentient-runtime-client`, not in git), a view-only WebGL test page in Tauri, shipped as an **Arch package** and run on
**Linux/WebKitGTK**. It hung at 100 % CPU and took his system down on open. It had never run on a real GPU before, and
used an 8192 px texture atlas. **Cause: unknown, must re-test.** No log or core dump was kept. The Windows Client uses a
different engine (WebView2 = Chromium, the same as Edge), so the result does not carry over, but it has never had a
stability test either: phase 0.

## 9. Known WebKitGTK problems (why Linux stays on a browser)

| Problem | Source |
|---|---|
| The runtime test client hung at 100 % CPU and crashed the owner's Arch system; cause unknown | memory note, 2026-11-06 |
| `WebKitWebProcess` SIGABRT on close with the DMABuf renderer (Mesa libgallium 26.x, Arch); master re-enabled DMABuf "for testing" | commit a02a358e, `tauri-app/src/main.rs` |
| Keyboard input fails silently when IBus is not running; worked around with `GTK_IM_MODULE=gtk-im-context-simple` | a02a358e |
| GPU compositing has to be forced (`WEBKIT_FORCE_COMPOSITING_MODE=1`); driver-dependent | 6f7a18eb |
| `position: sticky; bottom: 0` sticks to the content box: content scrolls under footers | SPM (old Tauri UI), 2026-09-12 |
| A generic `input { min-width }` turns drawn checkboxes and radios into 120 px pills | same |
| None of these show in Chromium, so headless Chrome tests do not catch them; the UI is tested on Chromium only | same |

## 10. Phased plan

| Phase | Work | Effort |
|---|---|---|
| P0 | Stability test of today's Client on Windows against Edge (section 7, short form). Stop if it is worse than Edge | 2 d |
| P1 | Client C1-C7, rename to "SENTIENT Client", version 0.2.0 | 7 d |
| P2 | CI Windows-only + Authenticode, `publish-sentient-client.sh`, manifest, README row, first pre-release | 2.5 d (+ certificate lead time) |
| P3 | SPM core: release lookup, `client.rs`, WebView2, pack kinds and pack script | 3.5 d |
| P4 | SPM UI: Setup choice and Summary, Client-only flow, launch with fallback, Platform settings card, Startup choice, Update row | 4.5 d |
| P5 | Full stability test, then the owner's hardware | 3 d + elapsed |

## 11. Risks

| Risk | Mitigation |
|---|---|
| The Client is no better than Edge's kiosk (same engine) | P0 decides; its value is no close/no escape, a single instance and crash reload |
| Unknown crash cause repeats in another form | P0 and P5 on real GPUs, his hardware last; keep the browser fallback |
| No code-signing certificate yet (SPM is unsigned too) | SmartScreen warnings until bought; EV or cloud HSM; the publish script refuses unsigned files once a certificate exists |
| Embedded UI drifts from the server | C1 (remote URL) removes it |
| OS-level escapes (Ctrl+Alt+Del, Win key) | Assigned Access / Shell Launcher documented per Windows edition; not promised by the app |
| Air-gapped WebView2 never updates | newer packs carry newer Standalone installers |
| WebView2 missing on LTSC with no pack | online bootstrapper; else a clear message and the browser fallback |

## 12. Open decisions for the owner

| # | Decision | Recommendation |
|---|---|---|
| D1 | Own `client-<ver>` release with its own manifest, or inside SENTIENT's | own release |
| D2 | Remote-URL shell or keep the embedded UI | remote-URL shell |
| D3 | WebView2 Evergreen Standalone or Fixed Version | Evergreen Standalone |
| D4 | Kiosk exit: key chord + password, password only, or none (sign out from Windows) | chord + password set in SPM |
| D5 | Client-only PCs also get the Platform Manager (for updates/removal), or the Client alone | with the Platform Manager |
| D6 | Client self-update (Tauri updater) or SPM only | SPM only |
| D7 | Tag prefix in the SENTIENT repo: keep `desktop-v*` or move to `client-v*` | `client-v*` |
| D8 | Which code-signing certificate (OV, EV token, cloud HSM) | shared with SPM |
| D9 | When the Client is chosen but not installable (offline, no pack file): stop, or fall back to Edge silently | fall back, with a Summary line saying so |

## Decisions (owner, 2026-11-19)

Taken as recommended: D1 own `client-<ver>` release with its own signed
manifest; D2 the Client loads the SENTIENT address it is given; D3 WebView2
Evergreen (bootstrapper online, Standalone installer in the offline pack);
D4 kiosk exit = key chord + password set in SPM; D5 Client-only PCs also
get the Platform Manager; D6 only SPM updates the Client; D7 tags
`client-v*`; D9 fall back to Edge when the Client cannot be installed.
Windows only. In the Update list the Client sits right after SENTIENT.
Open: D8, which code-signing certificate.
