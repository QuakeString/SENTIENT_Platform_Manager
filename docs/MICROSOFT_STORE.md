# The Platform Manager in the Microsoft Store

The SENTIENT Platform Manager is published in the Microsoft Store as an MSIX
package. Microsoft signs the package, so Windows trusts it without a paid
code-signing certificate. The GitHub download
(`SENTIENT-Setup-manager-<version>.exe`, self-updating, signed with INVENIA's
own certificate) stays as it is. Both are the same program: it notices at run
time that it was started from a Store package. There is no separate build
switch.

- Code: `app/src/store.rs` (the design is described at the top of that file)
- Package template and logos: `packaging/msix/` (`AppxManifest.xml`,
  `Assets/`, `make-assets.py`)
- Packaging: `scripts/package-msix.ps1`
- CI: the "MSIX (Store)" job in `.github/workflows/build-installers.yml`,
  artifact `msix-store`

## What is different in the Store build

| | GitHub download | Microsoft Store |
|---|---|---|
| Platform Manager updates | Update page, "Update and restart" | The Store. The Update page says "Updates come from the Microsoft Store." |
| SENTIENT and gateway updates | Update page | Update page (no change) |
| Where the program lives | `C:\Program Files\SENTIENT Platform Manager` | The package, plus a helper copy in `%ProgramData%\SENTIENT\Platform Manager\app` |
| Start menu entry and `spm` command | Made by Setup | From the package: Start entry, and the command `sentient-platform-manager` |
| Apps & features entry | Made by Setup | Listed by Windows for the package |
| Desktop shortcut to the Platform Manager | Optional in Setup | Not made: pin it from Start |
| Removing the Platform Manager | Its own switch in Uninstall | No switch. Uninstalling SENTIENT (Repair and uninstall > Uninstall) also removes the helper copy, the tray's sign-in task and the empty staging folder once nothing uses them. The helper is the program that is running, so its folder is deleted by a PowerShell that waits for it to close (outcome in the activity log). Without SENTIENT, the same page offers "Remove the helper". The app itself is uninstalled from Start (right-click, Uninstall) or Settings > Apps. |
| Administrator rights | UAC prompt | UAC prompt (the same) |

### How it works

A program inside a Store package cannot do what the Platform Manager needs
to do:

- The package folder (`C:\Program Files\WindowsApps\<package>`) is read-only,
  and its path changes with every Store update. Services, scheduled tasks,
  the operator screen and the RunOnce entry that continues Setup after the
  WSL restart need a program path that stays the same.
- A process with the package's identity has its writes to `HKCU` and to the
  user's AppData redirected into a private copy that only the package sees.
  The RunOnce entry would then never run. The only way to turn this off is
  the restricted capability `unvirtualizedResources`, which the Store build
  does not declare.

The packaged program therefore only hands over. The work is done by a
**helper**: a copy of the same program in
`%ProgramData%\SENTIENT\Platform Manager\app`, which runs outside the
package. That folder is locked so that only SYSTEM and the Administrators can
change it, and every user can read and run it (the ClickHouse account and the
operator screen run it too). This matters because ProgramData normally lets
every user add files, and one of SENTIENT's services runs this program as
LocalSystem.

What happens when the app is started from Start:

1. The packaged program reads `helper-build.txt` next to the helper. If the
   helper is the same build, the packaged program starts it as administrator
   (the normal UAC prompt, `ShellExecute` with `runas`) and exits. If a
   Platform Manager is already running (tray or window), the request goes to
   that copy instead, with no prompt.
2. If the helper is missing or older (the Store has updated the app), the
   packaged program copies itself to
   `%USERPROFILE%\.sentient-platform-manager\store-staging\<id>\spm-store-stage.exe`
   and starts that copy as administrator with `--store-stage`. The copy is
   in the user's own profile because other users cannot change files there.
   In ProgramData, another user could swap the file before it runs as
   administrator. The staged copy creates and locks the helper folder,
   replaces the helper (stopping and restarting the ClickHouse shutdown
   service if it runs, as the download's own update does), starts the
   helper with the original arguments, and exits. The helper deletes the
   staged copies the next time it starts. If an elevated window is already
   open (it might be running a job), the helper is not replaced under it.
   The request goes to that window, and the helper is updated at a later
   start.
3. From then on everything runs from the helper: the window, the tray's
   sign-in task, the RunOnce `--resume` entry written during Setup
   (`"<helper>" --resume`), the ClickHouse shutdown service, backups, and the
   operator screen. The helper is outside the package, so its registry and
   file writes land in the real places.

If the sign-in after the WSL restart does not continue Setup (the UAC
prompt of the RunOnce entry was declined or timed out), the next start of
the app from Start opens Setup on its install step and continues the saved
install. A plan older than three days, or one whose continued run already
stopped once, is offered with a "Continue the SENTIENT install" button on
the first page instead of opening by itself. The saved plan and its
passwords are removed only when the install finishes, or when a new install
is started from the beginning.

The packaged process itself starts no command prompt or PowerShell. It only
copies a file, reads the running copy's lock file, and calls `ShellExecute`.
The elevated helper may start them, just as the download does.

Where things are written (all outside the package, in both builds):
`%ProgramData%\SENTIENT` (installation record, settings, the activity log,
the release-list cache, written-out server agents), the secret store under
`%ProgramData%\SENTIENT\secrets`, and SENTIENT's own folders. Nothing is
written next to the program. The kiosk browser profile is in
`%LOCALAPPDATA%\SENTIENT`, written by the helper (outside the package), so it
is not redirected.

Known limits:

- The tray started at sign-in runs the helper. After a Store update it keeps
  running the older helper until the app is next opened from Start, which
  updates the helper.
- Windows gives a Store app no way to run anything when it is uninstalled.
  Uninstalling the app from Start or Settings > Apps (or
  `Remove-AppxPackage`) removes only the package: the helper folder
  `%ProgramData%\SENTIENT\Platform Manager\app` and, if SENTIENT is still
  installed, SENTIENT itself stay (SENTIENT's services need the helper, and
  the tray keeps working from it). The clean way out is: in the app,
  Repair and uninstall > Uninstall (this removes SENTIENT and the helper),
  then uninstall the app. A helper left behind can be deleted by hand as
  administrator once no SENTIENT service uses it.
- Over-the-shoulder elevation (a standard user typing an administrator's
  credentials) works as it does for the download. The window then runs as
  that administrator.

### Checking where writes land: `--job package-identity`

A hidden check, used to verify the design on a real Windows:

```
sentient-platform-manager.exe --job package-identity packaged
"C:\ProgramData\SENTIENT\Platform Manager\app\SENTIENT Platform Manager.exe" --job package-identity helper
```

Run the first from a normal command prompt (the app execution alias starts
the packaged program). Run the second from an administrator command prompt.
Each run reports:

- whether `GetCurrentPackageFullName` returns a package, and which;
- what `%ProgramData%\SENTIENT\msix-probe.txt` held before, writes it, reads
  it back, and shows its real path (`GetFinalPathNameByHandle`). A path under
  `...\Packages\...\LocalCache` means the write was redirected;
- the same for `HKLM\SOFTWARE\SENTIENT\MsixProbe` and
  `HKCU\SOFTWARE\SENTIENT\MsixProbe`, with the key's name as the process
  sees it (`\REGISTRY\A\...` means a private hive). A packaged process's
  HKCU write can land in the package's private hive (`SystemAppData\Helium`)
  while the name still reads as the user's own key, so for a packaged run
  the probe only says it may be private: the helper run, or `reg query`
  from a normal command prompt, shows whether the value is really there;
- whether the package's private `LocalCache` and `SystemAppData\Helium`
  folders exist, and any `msix-probe.txt` copy in `LocalCache`.

The report is also saved as `%ProgramData%\SENTIENT\msix-probe-report-<pid>.txt`.
Each run shows what the previous run left, so running them one after the
other shows whether the other run's writes landed for real. Expected: the
packaged run has a package name, and its HKCU write lands in a private hive
(HKLM fails without administrator rights). The helper run has no package
name, and all three writes land in the real places.

## Partner Center: the package identity

The app "SENTIENT Platform Manager" is reserved in Partner Center (product
type MSIX or PWA app). Product management > Product identity:

| Field | Value |
|---|---|
| Package/Identity/Name | `INVENIASYSTEMS.SENTIENTPlatformManager` |
| Package/Identity/Publisher | `CN=8CCEC686-FD1B-4801-ABEE-6B4CE6A2E92C` (Microsoft's publisher ID, not INVENIA's certificate) |
| Package/Properties/PublisherDisplayName | `INVENIA SYSTEMS` |
| Package Family Name | `INVENIASYSTEMS.SENTIENTPlatformManager_sjyzajq1qbzj2` |
| Store ID | `9PP6JLZV7S2R` (https://apps.microsoft.com/detail/9PP6JLZV7S2R) |

The packaging script builds with this identity by default. The repository's
Actions variables `MSIX_IDENTITY_NAME`, `MSIX_PUBLISHER` and
`MSIX_PUBLISHER_DISPLAY_NAME` override it (not needed today).

`-TestIdentity` builds a test identity instead
(`INVENIASYSTEMS.SENTIENTPlatformManager.Test`,
`C=IN, O=INVENIA SYSTEMS, CN=INVENIA SYSTEMS`, the certificate's subject in the order Windows compares it) for sideloading with
INVENIA's own certificate. It is never submitted.

## Building

CI: every build of `build-installers` (tags, nightly, manual runs, and pull
requests that change `packaging/msix/` or the packaging script) runs
"MSIX (Store)". The `msix-store` artifact contains:

- `store/SENTIENT-Platform-Manager-<v>.msix` and `.msixupload`: unsigned,
  with the Partner Center identity. This is what you submit.
- `sideload/SENTIENT-Platform-Manager-<v>.msix`: the test identity, signed
  with INVENIA's code-signing certificate when the repository has it
  (secrets `SPM_SIGN_PFX_BASE64` and `SPM_SIGN_PFX_PASSWORD`).

The Store takes final versions only, with a four-part number whose last part
is 0. 0.9.4 becomes 0.9.4.0. A `v0.9.4` tag packages 0.9.4.0. Release
candidates, nightlies and manual builds are packaged with the version in
`app/Cargo.toml`, for testing (the program inside still reports its own
version). A pre-release version given to the script is refused.

Locally, on Windows with the Windows 10/11 SDK installed:

```powershell
cd app; cargo build --release; cd ..
powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1 `
  -Exe app\target\release\sentient-platform-manager.exe -OutDir dist
```

The script lays out the package, fills `AppxManifest.xml` through the
program's `--job msix-manifest`, runs `makepri` (so Windows picks the logo
that fits each screen scale) and `makeappx pack`, and zips the `.msix` into
the `.msixupload`. A `.msixupload` for a single x64 package is a zip that
holds the `.msix` (Visual Studio adds symbol files, which are optional).
Partner Center also accepts the `.msix` on its own.

After the app icon changes, make the logos again with
`python3 packaging/msix/make-assets.py` and commit `packaging/msix/Assets`.

## Sideload test

1. Trust INVENIA's certificate on the test computer, as administrator:
   `powershell -ExecutionPolicy Bypass -File packaging\windows\Trust-INVENIA.ps1`
   (adds it to Trusted Root and Trusted Publishers). Alternatively:
   `Import-Certificate -FilePath packaging\windows\INVENIA-SYSTEMS-codesign.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople`.
2. Use the CI's `sideload\...msix` (signed). If it is unsigned, sign it on
   a computer with the Windows SDK:
   ```powershell
   $env:MSIX_SIGN_PFX_PASSWORD = Read-Host "pfx password"   # not on a command line
   powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1 `
     -Exe sentient-platform-manager.exe -TestIdentity -SignWith C:\keys\invenia.pfx
   ```
   The certificate's subject must equal the package's Publisher. The test
   identity's Publisher is INVENIA's certificate subject.
3. Install: `Add-AppxPackage .\SENTIENT-Platform-Manager-0.9.4.msix`
4. Start "SENTIENT Platform Manager" from Start.
5. Remove: right-click it in Start and choose Uninstall, or
   `Get-AppxPackage *SENTIENTPlatformManager* | Remove-AppxPackage`.

## Submitting

1. Partner Center > the app > Start a submission.
2. Packages: upload `store/SENTIENT-Platform-Manager-<v>.msixupload` (or the
   `.msix`). Device family: Windows 10/11 Desktop.
3. Properties: category Developer tools (or Business), and declare that the
   app depends on non-Microsoft drivers or software (WSL 2 and Ubuntu,
   Docker Engine, PostgreSQL, SENTIENT).
4. Store listing: the description below.
5. Submission options, Restricted capabilities: the runFullTrust
   justification below. Notes for certification: below.

### runFullTrust justification

The SENTIENT Platform Manager is a full-trust desktop app (Win32, Rust) that
installs and manages the SENTIENT server on this computer (Windows services,
PostgreSQL, WSL 2 with Docker Engine), so it needs to run as a regular
desktop program.

### Store description (draft)

> **Requires other software.** The SENTIENT Platform Manager installs and
> manages SENTIENT, an industrial IoT platform, on this computer. Setting up
> SENTIENT downloads and installs other software: the SENTIENT server,
> PostgreSQL (with TimescaleDB), and, depending on the installation you
> choose, WSL 2 with an Ubuntu distribution and Docker Engine, and the
> SENTIENT gateway. Administrator rights are needed for the installation.
> SENTIENT itself needs a licence from INVENIA SYSTEMS.
>
> With the Platform Manager you can:
> - install SENTIENT on this computer in a few steps, natively or in Docker
>   on WSL 2, with PostgreSQL, TimescaleDB or ClickHouse;
> - start, stop and watch SENTIENT and its database, and read their logs;
> - update SENTIENT and the SENTIENT gateway, and roll an update back;
> - back up and restore SENTIENT's data, also on a schedule;
> - manage SENTIENT on your Linux and Windows servers over SSH;
> - set up an operator screen that opens SENTIENT full screen at sign-in;
> - remove SENTIENT again, completely or keeping its data.

### Notes for certification (draft)

> - The app installs other software outside its package, by design and only
>   when the user asks: the SENTIENT server and its database as Windows
>   services, WSL 2 with an Ubuntu distribution and Docker Engine, and, if
>   chosen, the SENTIENT gateway. Every installation step asks for
>   administrator rights through the normal UAC prompt.
> - To run these services and its own elevated steps, the app keeps a copy
>   of its own program in C:\ProgramData\SENTIENT\Platform Manager\app (only
>   administrators can change it). Turning on WSL 2 needs one Windows restart.
>   Setup then continues by itself after sign-in.
> - When the user adds a remote server, the app copies its own server agent
>   (part of this package) to that server over SSH, with the user's SSH
>   credentials.
> - The app does not update itself in the Store build: updates come from the
>   Store. It does download SENTIENT and gateway releases from INVENIA's
>   GitHub releases and checks each against its published SHA-256 and
>   signature.
> - Removal: in the app, Repair and uninstall > Uninstall removes SENTIENT
>   and everything the app installed, including its own helper copy in
>   C:\ProgramData\SENTIENT\Platform Manager\app. Do this before
>   uninstalling the app: uninstalling the app from the Store removes only
>   the app (Windows runs nothing of the app's at that point), so with
>   SENTIENT still installed its services and the helper they use stay.
> - How to test: start the app, accept the UAC prompt, and choose Install
>   SENTIENT with the default options. It needs internet access and takes
>   several minutes. Open SENTIENT from the app, then uninstall it (Repair
>   and uninstall > Uninstall). The Docker and ClickHouse options need WSL 2,
>   which needs virtualisation turned on and one Windows restart.
>   [Before submitting: say here whether the reviewer needs a SENTIENT
>   licence key to get past SENTIENT's sign-in, and give a test key if so.]

## Package manifest notes

- `TargetDeviceFamily Windows.Desktop`, `MinVersion 10.0.19041.0` (Windows 10
  2004, also the oldest WSL 2 the app supports), `MaxVersionTested
  10.0.26100.0`.
- Capability `runFullTrust` only. The Store build declares no
  `allowElevation`, `packagedServices`, `localSystemServices` or
  `unvirtualizedResources`. It elevates the way the download does, with the
  UAC prompt, from the helper outside the package.
- No firewall rules in the manifest. The app itself listens only on
  127.0.0.1 (the tray's hand-over). SENTIENT's Windows Firewall rules are made
  by the elevated helper during installation, for SENTIENT's own ports, as
  in the download. If the app ever needs a rule of its own, it goes in a
  `desktop2:Extension Category="windows.firewallRules"`.
- App execution alias `sentient-platform-manager.exe`. It starts the
  packaged program by name, with arguments.
