# Release signing

How SENTIENT and Platform Manager releases are signed, and how the Platform
Manager checks them before it uses a download.

## What is signed

| File | Signature | Covers |
|---|---|---|
| A SENTIENT release's `manifest.json` | `manifest.json.minisig` | Every file the manifest lists, through the SHA-256 it gives each one |
| Each Platform Manager installer (`SENTIENT-Setup-manager-*`) | `<installer>.minisig` | The installer itself |

The signatures use minisign's format with ed25519. GitHub's SHA-256 shows that a
file arrived as it was uploaded. The signature shows that the holder of the
release key published it.

## The pinned public key

```
untrusted comment: SENTIENT release signing key 2026-11 (minisign public key 0A8C8F7EFCE2CBA2)
RWSiy+L8fo+MCtWNaQPLK8KSB3WBwoVVaMaxcLeUE8m8HuvlaJL6p0Ev
```

- Key id: `0A8C8F7EFCE2CBA2`
- In the code: `RELEASE_PUBLIC_KEY` in
  `crates/sentient-installer-core/src/signing.rs`, built into every Platform
  Manager.
- As a file: `packaging/release-signing.pub`. `scripts/sign-release.sh` checks
  every signature it writes against this file.

## Where the secret key is

- Only on the release machine (st-dev), in `~/.sentient-keys/release-2026-11/`.
  The folder is mode 0700. `release.key` (secret) and `release.pub` are 0600.
- It is never committed to any repository and never printed.
- It is not copied anywhere else in clear text: no backup, no CI secret, no
  shared drive. A copy in clear text elsewhere counts as a leak: rotate the key
  (see below).

### No passphrase: the decision and why

The secret key has no passphrase, on purpose. Publishing runs unattended,
from scripts and agents on the release machine, and a passphrase prompt would
stop that. The key is protected instead by:

- file permissions (0600 in a 0700 folder, one user);
- living on one machine only;
- never being copied in clear text.

The trade-off: anyone who can read files as that user on st-dev can sign
releases. If that machine or account is ever compromised, rotate the key.

## How to sign

1. Install a signer once: `cargo install rsign2` (provides `rsign`), or have
   `minisign` on PATH.
2. Sign: `scripts/sign-release.sh <file>...`. For each file the script:
   - writes `<file>.minisig`, with trusted comment `file:<name>`;
   - verifies it against `packaging/release-signing.pub`;
   - refuses to run when the key file is not mode 0600 or 0400.

   `SENTIENT_RELEASE_KEY` overrides the key path. The default is
   `~/.sentient-keys/release-2026-11/release.key`.
3. The publish scripts already call it:
   - `scripts/publish-sentient-release.sh` signs `manifest.json` and uploads
     `manifest.json.minisig`. When files are added to an existing release, it
     replaces both.
   - `scripts/publish-spm-to-sentient-release.sh` signs the
     `SENTIENT-Setup-manager-*` installers and uploads their `.minisig` files.

To check a file by hand:

```
rsign verify -p packaging/release-signing.pub -x <file>.minisig <file>
```

## What the Platform Manager does with a signature

The checks live in `crates/sentient-installer-core/src/signing.rs`. They run in
two places:

- `app/src/online.rs`: every native SENTIENT download, through
  `manifest::fetch_signed`;
- `app/src/updates.rs`: the Platform Manager self-update, before the
  downloaded installer runs.

The rule is applied per product (SENTIENT, Platform Manager):

| The release has… | Signing has not begun | Signing has begun |
|---|---|---|
| A valid signature | Used | Used |
| A wrong signature (other bytes, other key, damaged) | Refused | Refused |
| No signature | Used with an "Unsigned release" warning | Refused |

Signing has begun for a product as soon as either of these is true:

- the release feed (github.com/QuakeString/sentient-release) holds a
  non-draft release of that product with a signature. For SENTIENT that means
  `manifest.json.minisig`; for the Platform Manager, any `.minisig` on an
  `spm-v*` release;
- this computer has seen such a release before.

The second condition is remembered with a marker file in the Platform
Manager's settings folder (`signed-releases-seen-sentient`,
`signed-releases-seen-manager`). A feed that later hides its signatures
therefore cannot reopen the unsigned path.

On the Update page:

- an unsigned offer shows "Unsigned release" before signing has begun;
- once signing has begun it shows "Not signed", and its update button is off.

Docker installs pull the image and are not covered by these signatures.

## Authenticode (Windows)

On Windows, a downloaded Platform Manager installer also passes WinVerifyTrust
before it runs (`app/src/authenticode.rs`). This is on top of the release
signature.

| Authenticode result | Decision |
|---|---|
| Valid signature from a trusted publisher | Runs |
| Untrusted root, signer is exactly a pinned self-signed certificate (`PINNED_SIGNERS`) | Runs |
| Signature present but the file changed after signing (bad digest) | Refused |
| Certificate revoked, expired, untrusted root or explicitly distrusted | Refused |
| Other verification error | Refused (the error code is shown) |
| No Authenticode signature at all | Runs, with a warning, while `REQUIRE_SIGNATURE` is `false` |

Missing signatures are allowed for now because Platform Manager releases are
not yet code-signed: `scripts/sign-windows.ps1` leaves them unsigned when no
certificate is configured. Refusing them would stop every Windows update. Once
releases carry a code-signing certificate, set `REQUIRE_SIGNATURE` to `true`
in `app/src/authenticode.rs`; a missing signature is then refused too. The
publisher's name is not pinned.

### Code signing certificate: self-signed INVENIA SYSTEMS (for now)

Until a public code-signing certificate is bought, Windows programs are signed
with our own self-signed certificate:

- Subject `CN=INVENIA SYSTEMS, O=INVENIA SYSTEMS, C=IN`, RSA 3072, SHA-256,
  code signing only, valid 2026-11-19 to 2031-11-19, SHA-1 thumbprint
  `C7DC8013F1ECD97ED20193F04E56359B17B24CBD`.
- The public certificate: `packaging/windows/INVENIA-SYSTEMS-codesign.cer`.
  Its SHA-256 is `PINNED_SIGNERS` in `app/src/authenticode.rs` (a test keeps
  the two equal).
- The key: `~/.sentient-keys/codesign-selfsigned-2026-11/codesign.pfx` and its
  password file next to it, on the release machine only, mode 0600, like the
  release key. Never in a repository, a CI secret or a log.

No computer trusts it by itself: WinVerifyTrust answers "untrusted root", UAC
shows "Publisher: Unknown", and SmartScreen behaves as for an unsigned file.
What it gives now: the Platform Manager accepts it through the pin, a changed
file is caught (bad digest), and a managed site can trust it:
`packaging/windows/Trust-INVENIA.ps1` (as administrator; `-Remove`, `-Status`)
adds the `.cer` next to it to the computer's Trusted Root and Trusted
Publishers, after which UAC shows "Verified publisher: INVENIA SYSTEMS".
Group Policy can deploy the same `.cer`. UAC remembers a file it has shown: a
file approved while trusted still shows "Verified" after `-Remove`.

What is signed:

| Program | Where | From |
|---|---|---|
| SENTIENT `bin/sentient-server.exe`, `sentient-install.exe`, `sentient-admin.exe` | `sentient-payload.tar` | 4.4.1.1: `scripts/sign-sentient-payload.sh <payload.tar>`; `publish-sentient-release.sh` refuses an unsigned one while the certificate is on the machine |
| Gateway program and `SentientGatewaySetup-<v>.exe` | sentient-gateway-rs `packaging/windows/build.sh` | the next gateway release; `publish-gateway-to-sentient-release.sh` refuses an unsigned installer the same way |
| SENTIENT Client installer | SENTIENT `tauri-build.yml` artifact, signed on the release machine | the next Client build |
| Platform Manager / SENTIENT Setup (one exe) and its Windows agent | `build-installers.yml` (`scripts/sign-windows.ps1`) | 0.9.4: 0.9.3 is the first release with the pin, so 0.9.3 itself stays unsigned (older Platform Managers refuse an untrusted root) |

`SENTIENT_ALLOW_UNSIGNED=1` lets the publish scripts go ahead without a
signature (say why in the notes).

Signing from Linux: `scripts/sign-windows.sh [--desc "Name"] <file>...`
(osslsigncode, from PATH or a throwaway `debian:12` container) with an
RFC 3161 timestamp (`http://timestamp.digicert.com`), so a signature stays
valid after the certificate expires; `--check` only verifies. On Windows CI:
`scripts/sign-windows.ps1` with the `SPM_SIGN_PFX_BASE64` /
`SPM_SIGN_PFX_PASSWORD` secrets (not set: the key stays off GitHub for now).

A public certificate later: point `SENTIENT_CODESIGN_PFX` (or the default in
`sign-windows.sh`) at it, or `SPM_SIGN_THUMBPRINT` for a token or cloud key.
It needs no pin; keep the self-signed pin until no installed Platform Manager
can meet a self-signed release.

## Rotating the key

Rotate on a schedule, or at once if the key may have leaked.

1. Generate a new pair in a new folder:
   ```
   umask 077
   mkdir -p ~/.sentient-keys/release-YYYY-MM && chmod 700 ~/.sentient-keys/release-YYYY-MM
   rsign generate -W -p ~/.sentient-keys/release-YYYY-MM/release.pub -s ~/.sentient-keys/release-YYYY-MM/release.key
   chmod 600 ~/.sentient-keys/release-YYYY-MM/*
   ```
2. Put the new public key in `packaging/release-signing.pub` and in
   `RELEASE_PUBLIC_KEY` (`signing.rs`). Change the default path in
   `scripts/sign-release.sh`, and update this document.
3. Release a Platform Manager that pins the new key. Sign that release with the
   **old** key: the Platform Managers in the field still trust only the old key.
4. From then on, sign with the new key. Platform Managers older than step 3
   reject releases signed with the new key, so they must update through step 3
   first.
5. Delete the old secret key (`shred -u`). If the old key leaked, also re-sign
   the current releases with the new key and replace their `.minisig` files.
