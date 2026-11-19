// Copyright © 2016-2026 The SENTIENT Authors

//! The offline pack: one folder (unzipped from
//! `SPM-offline-installer-<spm>-SENTIENT-<ver>-windows-x64.zip`) that holds
//! everything an install or an update needs, so nothing is downloaded
//! (docs/analysis/OFFLINE_INSTALLER_STUDY.md).
//!
//! `pack.json` lists every file with its SHA-256, size and kind, and the
//! versions it carries. `pack.json.minisig` signs it with the release key
//! ([`crate::signing::RELEASE_PUBLIC_KEY`]). A pack has no "before signing"
//! grace: unsigned or badly signed, it is refused. The signature covers
//! `pack.json` only; each file is covered by its SHA-256 there, checked by
//! [`Opened::verify_files`] before anything on the computer changes.
//!
//! Order of use: [`open`] (signature), [`check_target`] (this computer),
//! [`Opened::verify_files`] (every file, streamed), then
//! [`crate::offline::activate`], after which every downloader refuses the
//! network and the install takes its files from the pack.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The pack's manifest, at the pack's root.
pub const PACK_FILE: &str = "pack.json";
/// Its signature, beside it.
pub const SIGNATURE_FILE: &str = "pack.json.minisig";
/// The newest `format` this Platform Manager reads.
pub const FORMAT: u32 = 1;
/// The one target so far.
pub const TARGET_WINDOWS_X64: &str = "windows-x64";

/// What a file in the pack is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The Platform Manager's own program (`SENTIENT-Setup.exe`).
    Spm,
    /// `sentient-payload.tar`: the native SENTIENT.
    Payload,
    /// The PostgreSQL binaries zip.
    Pg,
    /// The TimescaleDB zip.
    Tsdb,
    /// `vc_redist.x64.exe`.
    Vcredist,
    /// The gateway's Windows installer.
    Gateway,
    /// Microsoft's WSL package (Windows 11, Windows 10 .2311+).
    WslMsi,
    /// The WSL 2 kernel for an older Windows 10's inbox WSL.
    KernelMsi,
    /// The Ubuntu WSL root file system (`wsl --import`).
    Rootfs,
    /// A Docker Engine package for that Ubuntu; installed in list order.
    Deb,
    /// A `docker save` of one image (`image_ref`).
    Image,
    /// Anything else (VERSION, NOTES.md, EULA): checked, never run.
    #[serde(other)]
    Other,
}

/// One file of the pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackFile {
    /// Relative to the pack's root, `/`-separated.
    pub path: String,
    /// Lower-case hex.
    pub sha256: String,
    pub size: u64,
    pub kind: Kind,
    /// For an image: the reference it loads as (`quakestring/sentient:4.4.0.4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

/// The oldest operating system the pack runs on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MinOs {
    /// Windows' build number (19044: Windows 10 21H2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_build: Option<u32>,
}

/// The versions of what the pack carries besides the three products.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Components {
    #[serde(default)]
    pub postgresql: Option<String>,
    #[serde(default)]
    pub timescaledb: Option<String>,
    #[serde(default)]
    pub wsl: Option<String>,
    #[serde(default)]
    pub ubuntu_rootfs: Option<String>,
    #[serde(default)]
    pub docker: Option<String>,
}

/// `pack.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pack {
    pub format: u32,
    pub target: String,
    #[serde(default)]
    pub min_os: MinOs,
    pub spm_version: String,
    pub sentient_version: String,
    /// SENTIENT's build version, what a licence is compared with
    /// (crate::licence_version::build_version); else from `sentient_version`.
    #[serde(default)]
    pub build_version: Option<String>,
    #[serde(default)]
    pub gateway_version: Option<String>,
    #[serde(default)]
    pub components: Components,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub notes: String,
    pub files: Vec<PackFile>,
}

/// Why a pack is not used. Every message says what to do, and that nothing
/// was changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No `pack.json` in the folder.
    NotAPack(PathBuf),
    /// `pack.json` is there but cannot be read or parsed.
    Unreadable(String),
    /// A `format` newer than this Platform Manager reads.
    NewerFormat(u32),
    Unsigned,
    BadSignature(String),
    /// The pack is for another system (`target`), this computer runs `here`.
    WrongOs { target: String, here: String },
    /// Windows x64 pack on another processor (`here`: ARM64 …).
    WrongArch { here: String },
    /// Windows older than the pack's minimum.
    OsTooOld { need: u32, here: u32 },
    /// A listed path that leaves the pack (absolute, `..`).
    BadPath(String),
    Missing(String),
    WrongSize { file: String, expected: u64, found: u64 },
    Changed(String),
    Cancelled,
}

impl Refusal {
    pub fn message(&self) -> String {
        const KEPT: &str = "Nothing was changed.";
        match self {
            Refusal::NotAPack(dir) => format!("{} is not an offline pack: it has no {PACK_FILE}. Choose the folder the pack's zip was unpacked into. {KEPT}", dir.display()),
            Refusal::Unreadable(e) => format!("The offline pack's {PACK_FILE} cannot be read ({e}). Copy the pack again. {KEPT}"),
            Refusal::NewerFormat(f) => format!("This offline pack (format {f}) is newer than this Platform Manager reads (format {FORMAT}). Run the SENTIENT-Setup.exe that came in the pack. {KEPT}"),
            Refusal::Unsigned => format!("This offline pack is not signed ({SIGNATURE_FILE} is missing), so where it came from cannot be checked. Use a pack downloaded from the SENTIENT releases. {KEPT}"),
            Refusal::BadSignature(e) => format!("This offline pack's signature is not valid ({e}): the pack may have been altered. Use a pack downloaded from the SENTIENT releases. {KEPT}"),
            Refusal::WrongOs { target, here } => format!("This offline pack is for {}; this computer runs {here}. Download the pack for this computer. {KEPT}", target_name(target)),
            Refusal::WrongArch { here } => format!("This offline pack is for Windows x64 (64-bit Intel or AMD); this computer has {here}. {KEPT}"),
            Refusal::OsTooOld { need, here } => format!("This offline pack needs Windows 10 build {need} or newer; this computer runs build {here}. Update Windows first. {KEPT}"),
            Refusal::BadPath(p) => format!("The offline pack lists a file outside its folder ({p}): it is not used. {KEPT}"),
            Refusal::Missing(f) => format!("The offline pack is incomplete: {f} is missing. Copy the pack again. {KEPT}"),
            Refusal::WrongSize { file, expected, found } => format!("The offline pack is damaged: {file} is {found} bytes, the pack says {expected}. Copy the pack again. {KEPT}"),
            Refusal::Changed(f) => format!("The offline pack is damaged: {f} does not match its checksum. Copy the pack again. {KEPT}"),
            Refusal::Cancelled => format!("The check of the offline pack was cancelled. {KEPT}"),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// A target's name as a sentence says it.
pub fn target_name(target: &str) -> String {
    match target {
        TARGET_WINDOWS_X64 => "Windows x64".into(),
        "linux-x64" | "linux-x86_64" => "Linux x64".into(),
        "linux-arm64" | "linux-aarch64" => "Linux ARM64".into(),
        other => other.to_string(),
    }
}

/// `pack.json`'s text as a [`Pack`]: its format known, its paths inside it.
pub fn parse(text: &str) -> Result<Pack, Refusal> {
    // The format first: a newer pack may not parse as this one.
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| Refusal::Unreadable(e.to_string()))?;
    let format = v.get("format").and_then(|f| f.as_u64()).ok_or_else(|| Refusal::Unreadable("no format".into()))? as u32;
    if format > FORMAT {
        return Err(Refusal::NewerFormat(format));
    }
    let pack: Pack = serde_json::from_value(v).map_err(|e| Refusal::Unreadable(e.to_string()))?;
    for f in &pack.files {
        if !path_inside(&f.path) {
            return Err(Refusal::BadPath(f.path.clone()));
        }
    }
    Ok(pack)
}

/// A listed path is relative and stays inside the pack.
fn path_inside(p: &str) -> bool {
    let path = Path::new(p);
    !p.is_empty()
        && !p.contains(':')
        && !p.starts_with(['/', '\\'])
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !p.split(['/', '\\']).any(|s| s == "..")
}

/// A pack whose signature was checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub dir: PathBuf,
    pub pack: Pack,
    /// The signature's trusted comment.
    pub signed: String,
}

/// Open the pack in `dir`: `pack.json` signed with the release key.
pub fn open(dir: &Path) -> Result<Opened, Refusal> {
    open_with_key(crate::signing::RELEASE_PUBLIC_KEY, dir)
}

/// [`open`] against `key` (tests sign with a key of their own).
pub fn open_with_key(key: &str, dir: &Path) -> Result<Opened, Refusal> {
    let file = dir.join(PACK_FILE);
    if !file.is_file() {
        return Err(Refusal::NotAPack(dir.to_path_buf()));
    }
    let bytes = std::fs::read(&file).map_err(|e| Refusal::Unreadable(e.to_string()))?;
    let signature = match std::fs::read_to_string(dir.join(SIGNATURE_FILE)) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Refusal::Unsigned),
        Err(e) => return Err(Refusal::Unreadable(format!("{SIGNATURE_FILE}: {e}"))),
    };
    let signed = crate::signing::verify_with(key, &bytes, &signature).map_err(Refusal::BadSignature)?;
    let text = String::from_utf8(bytes).map_err(|_| Refusal::Unreadable("not UTF-8".into()))?;
    Ok(Opened { dir: dir.to_path_buf(), pack: parse(&text)?, signed })
}

/// Where a pack is looked for without asking: the folder of the running
/// program (Setup unpacked from the pack's zip).
pub fn beside_exe() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join(PACK_FILE).is_file().then_some(dir)
}

/// This computer, as a pack's target check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// "windows", "linux", "macos".
    pub os: String,
    /// As a sentence says it: "Windows (build 22631)", "Ubuntu 24.04 LTS".
    pub os_name: String,
    /// The processor, not this program's build: "x86_64", "aarch64".
    pub arch: String,
    pub windows_build: Option<u32>,
}

impl Host {
    /// This computer.
    pub fn this() -> Host {
        let os = std::env::consts::OS.to_string();
        #[cfg(windows)]
        {
            let build = crate::wsl::os_build().map(|(b, _)| b);
            // An x64 program runs emulated on ARM64 Windows: the processor
            // is what Windows says, not what this program was built for.
            let reported = std::env::var("PROCESSOR_ARCHITEW6432").or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE")).unwrap_or_default();
            let arch = match reported.to_ascii_uppercase().as_str() {
                "ARM64" => "aarch64".to_string(),
                "AMD64" | "" => std::env::consts::ARCH.to_string(),
                "X86" => "x86".to_string(),
                other => other.to_ascii_lowercase(),
            };
            let os_name = match build {
                Some(b) if b >= 22000 => format!("Windows 11 (build {b})"),
                Some(b) => format!("Windows 10 (build {b})"),
                None => "Windows".into(),
            };
            Host { os, os_name, arch, windows_build: build }
        }
        #[cfg(not(windows))]
        {
            let name = crate::glibc::local_os_name();
            let os_name = if name.is_empty() { if os == "macos" { "macOS".into() } else { "Linux".into() } } else { name };
            Host { os, os_name, arch: std::env::consts::ARCH.to_string(), windows_build: None }
        }
    }

    fn arch_name(&self) -> String {
        match self.arch.as_str() {
            "aarch64" | "arm64" => "an ARM64 processor".into(),
            "x86" => "a 32-bit processor".into(),
            other => format!("a {other} processor"),
        }
    }
}

/// Whether the pack runs on `host`: its target, the processor, the
/// Windows build.
pub fn check_target(pack: &Pack, host: &Host) -> Result<(), Refusal> {
    match pack.target.as_str() {
        TARGET_WINDOWS_X64 => {
            if host.os != "windows" {
                return Err(Refusal::WrongOs { target: pack.target.clone(), here: host.os_name.clone() });
            }
            if host.arch != "x86_64" {
                return Err(Refusal::WrongArch { here: host.arch_name() });
            }
            if let (Some(need), Some(here)) = (pack.min_os.windows_build, host.windows_build) {
                if here < need {
                    return Err(Refusal::OsTooOld { need, here });
                }
            }
            Ok(())
        }
        other => Err(Refusal::WrongOs { target: other.to_string(), here: host.os_name.clone() }),
    }
}

/// Progress of [`Opened::verify_files`].
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    /// The file being read (its path in the pack).
    pub file: String,
    /// Files finished, of `files`.
    pub done_files: usize,
    pub files: usize,
    pub done_bytes: u64,
    pub bytes: u64,
}

impl Checked {
    pub fn fraction(&self) -> f32 {
        if self.bytes == 0 { 1.0 } else { (self.done_bytes as f64 / self.bytes as f64).clamp(0.0, 1.0) as f32 }
    }
}

/// "3.2 GB", "515 MB", "8 KB" (decimal, as a file manager shows them).
pub fn size_text(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} bytes")
    }
}

impl Pack {
    /// Bytes of every listed file.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// "Checking the offline pack: 41 files, 3.2 GB".
    pub fn check_title(&self) -> String {
        format!("Checking the offline pack: {} files, {}", self.files.len(), size_text(self.total_bytes()))
    }

    /// The build version a licence must cover for this pack's SENTIENT.
    pub fn licence_build(&self) -> String {
        crate::licence_version::build_version(self.build_version.as_deref(), &self.sentient_version)
    }
}

impl Opened {
    /// A file's place on disk.
    pub fn path_of(&self, f: &PackFile) -> PathBuf {
        f.path.split(['/', '\\']).fold(self.dir.clone(), |p, s| p.join(s))
    }

    /// The first file of `kind`.
    pub fn file(&self, kind: Kind) -> Option<&PackFile> {
        self.pack.files.iter().find(|f| f.kind == kind)
    }

    /// Every file of `kind`, in the pack's order.
    pub fn files(&self, kind: Kind) -> Vec<&PackFile> {
        self.pack.files.iter().filter(|f| f.kind == kind).collect()
    }

    /// The image file that loads as `image` (a reference such as
    /// `postgres:18-alpine`).
    pub fn image(&self, image: &str) -> Option<&PackFile> {
        self.pack.files.iter().find(|f| f.kind == Kind::Image && f.image_ref.as_deref() == Some(image))
    }

    /// The folder of the native payload: what
    /// [`crate::native::ArtifactSource::Bundled`] reads (the payload tar,
    /// the PostgreSQL and TimescaleDB zips and vc_redist sit together).
    pub fn payload_dir(&self) -> Option<PathBuf> {
        let f = self.file(Kind::Payload)?;
        self.path_of(f).parent().map(Path::to_path_buf)
    }

    /// The pack carries what a Docker-in-WSL install needs.
    pub fn has_docker(&self) -> bool {
        self.file(Kind::Rootfs).is_some() && !self.files(Kind::Deb).is_empty() && self.file(Kind::Image).is_some()
    }

    /// Check every listed file: there, of its size, of its SHA-256. Read
    /// in 1 MiB pieces (the pack is gigabytes); `progress` is told after
    /// each piece at most every 64 MiB and after each file. `cancelled`
    /// is asked between pieces.
    pub fn verify_files(&self, progress: &dyn Fn(&Checked), cancelled: &dyn Fn() -> bool) -> Result<(), Refusal> {
        let files = self.pack.files.len();
        let bytes = self.pack.total_bytes();
        // Missing or wrong-sized files first: no point hashing gigabytes
        // when one file is not there.
        for f in &self.pack.files {
            let path = self.path_of(f);
            let meta = std::fs::metadata(&path).map_err(|_| Refusal::Missing(f.path.clone()))?;
            if !meta.is_file() {
                return Err(Refusal::Missing(f.path.clone()));
            }
            if meta.len() != f.size {
                return Err(Refusal::WrongSize { file: f.path.clone(), expected: f.size, found: meta.len() });
            }
        }
        let mut state = Checked { file: String::new(), done_files: 0, files, done_bytes: 0, bytes };
        let mut buf = vec![0u8; 1 << 20];
        for f in &self.pack.files {
            state.file = f.path.clone();
            progress(&state);
            let mut file = std::fs::File::open(self.path_of(f)).map_err(|_| Refusal::Missing(f.path.clone()))?;
            let mut hasher = Sha256::new();
            let mut since = 0u64;
            loop {
                if cancelled() {
                    return Err(Refusal::Cancelled);
                }
                let n = file.read(&mut buf).map_err(|_| Refusal::Changed(f.path.clone()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                state.done_bytes += n as u64;
                since += n as u64;
                if since >= 64 << 20 {
                    since = 0;
                    progress(&state);
                }
            }
            let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if !got.eq_ignore_ascii_case(f.sha256.trim()) {
                return Err(Refusal::Changed(f.path.clone()));
            }
            state.done_files += 1;
            progress(&state);
        }
        Ok(())
    }

    /// Only that every file is there with its size (an already verified
    /// pack opened again by another process: the ClickHouse account's).
    pub fn check_present(&self) -> Result<(), Refusal> {
        for f in &self.pack.files {
            match std::fs::metadata(self.path_of(f)) {
                Ok(m) if m.is_file() && m.len() == f.size => {}
                Ok(m) if m.is_file() => return Err(Refusal::WrongSize { file: f.path.clone(), expected: f.size, found: m.len() }),
                _ => return Err(Refusal::Missing(f.path.clone())),
            }
        }
        Ok(())
    }

    /// Copy `pack.json`, its signature and the files `keep` selects into
    /// `to` at their paths there: the part of the pack another account
    /// must read (the ClickHouse account cannot read a user's folders), or
    /// what the Platform Manager keeps after Setup (the gateway installer).
    pub fn copy_part(&self, to: &Path, keep: &dyn Fn(&PackFile) -> bool) -> Result<Opened, String> {
        std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
        for name in [PACK_FILE, SIGNATURE_FILE] {
            std::fs::copy(self.dir.join(name), to.join(name)).map_err(|e| format!("copy {name}: {e}"))?;
        }
        let copy = Opened { dir: to.to_path_buf(), pack: self.pack.clone(), signed: self.signed.clone() };
        for f in self.pack.files.iter().filter(|f| keep(f)) {
            let dest = copy.path_of(f);
            // Already there from an earlier copy, with its size: kept.
            if std::fs::metadata(&dest).is_ok_and(|m| m.len() == f.size) {
                continue;
            }
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
            }
            std::fs::copy(self.path_of(f), &dest).map_err(|e| format!("copy {}: {e}", f.path))?;
        }
        Ok(copy)
    }

    /// A file's SHA-256 checked again right before it is used (a pack on a
    /// removable drive may change after the check).
    pub fn recheck(&self, f: &PackFile) -> Result<PathBuf, String> {
        let path = self.path_of(f);
        let got = crate::manifest::sha256_file(&path).map_err(|_| Refusal::Missing(f.path.clone()).message())?;
        if !got.eq_ignore_ascii_case(f.sha256.trim()) {
            return Err(Refusal::Changed(f.path.clone()).message());
        }
        Ok(path)
    }
}

/// Screens: a pack that is not on disk (SPM_DEBUG_PACK renders).
pub fn debug_fake() -> Opened {
    let file = |path: &str, size: u64, kind: Kind, image_ref: Option<&str>| PackFile { path: path.into(), sha256: String::new(), size, kind, image_ref: image_ref.map(String::from), image_digest: None };
    let mut files = vec![
        file("SENTIENT-Setup.exe", 148_000_000, Kind::Spm, None),
        file("payload/sentient-payload.tar", 515_000_000, Kind::Payload, None),
        file("payload/postgresql-18.6-1-windows-x64-binaries.zip", 344_000_000, Kind::Pg, None),
        file("payload/timescaledb-postgresql-18-windows-amd64.zip", 8_400_000, Kind::Tsdb, None),
        file("payload/vc_redist.x64.exe", 25_600_000, Kind::Vcredist, None),
        file("gateway/SentientGatewaySetup-4.16.2.exe", 11_000_000, Kind::Gateway, None),
        file("wsl/wsl.2.6.1.0.x64.msi", 240_000_000, Kind::WslMsi, None),
        file("wsl/wsl_update_x64.msi", 17_000_000, Kind::KernelMsi, None),
        file("wsl/ubuntu-noble-wsl-amd64-20261101.rootfs.tar.gz", 350_000_000, Kind::Rootfs, None),
        file("images/sentient-4.4.0.4.tar.zst", 450_000_000, Kind::Image, Some("quakestring/sentient:4.4.0.4")),
        file("images/timescaledb-2.26.1-pg18.tar.zst", 500_000_000, Kind::Image, Some("timescale/timescaledb:2.26.1-pg18")),
        file("images/postgres-18-alpine.tar.zst", 150_000_000, Kind::Image, Some("postgres:18-alpine")),
        file("images/clickhouse-24.8-alpine.tar.zst", 250_000_000, Kind::Image, Some("clickhouse/clickhouse-server:24.8-alpine")),
        file("images/sentient-gateway-rs-4.16.2.tar.zst", 90_000_000, Kind::Image, Some("quakestring/sentient-gateway-rs:4.16.2")),
    ];
    for i in 0..27 {
        files.push(file(&format!("wsl/docker-debs/package-{i}.deb"), 4_500_000, Kind::Deb, None));
    }
    Opened {
        dir: PathBuf::from(r"E:\SPM-offline-installer-0.9.4-SENTIENT-4.4.0.4-windows-x64"),
        pack: Pack {
            format: FORMAT,
            target: TARGET_WINDOWS_X64.into(),
            min_os: MinOs { windows_build: Some(19044) },
            spm_version: "0.9.4".into(),
            sentient_version: "4.4.0.4".into(),
            build_version: Some("4.4.0".into()),
            gateway_version: Some("4.16.2".into()),
            components: Components::default(),
            created: "2026-11-19".into(),
            notes: String::new(),
            files,
        },
        signed: "debug".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test key that signed tests/fixtures/pack/pack.json (not the
    /// release key).
    const TEST_KEY: &str = "RWQ4HGD0nr/FIBrVKoNy0AQ6aFgV+0CavkOBGM2yrylY2snDHWspzFUK";
    const PACK_JSON: &str = include_str!("../tests/fixtures/pack/pack.json");
    const PACK_SIG: &str = include_str!("../tests/fixtures/pack/pack.json.minisig");
    /// The listed files' contents (their SHA-256 and sizes are in PACK_JSON).
    const CONTENTS: [(&str, &str); 5] = [
        ("payload/sentient-payload.tar", "payload\n"),
        ("gateway/SentientGatewaySetup-4.16.2.exe", "gateway\n"),
        ("wsl/docker-debs/containerd.io.deb", "deb-containerd\n"),
        ("wsl/docker-debs/docker-ce.deb", "deb-docker-ce\n"),
        ("images/sentient-4.4.0.4.tar.zst", "image-sentient\n"),
    ];

    /// A fresh folder holding the fixture pack (its signature when `signed`).
    fn pack_dir(name: &str, signed: bool) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spm-pack-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PACK_FILE), PACK_JSON).unwrap();
        if signed {
            std::fs::write(dir.join(SIGNATURE_FILE), PACK_SIG).unwrap();
        }
        for (path, text) in CONTENTS {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    fn windows(build: u32) -> Host {
        Host { os: "windows".into(), os_name: format!("Windows 11 (build {build})"), arch: "x86_64".into(), windows_build: Some(build) }
    }

    #[test]
    fn a_signed_pack_parses_with_its_versions_and_files() {
        let dir = pack_dir("good", true);
        let p = open_with_key(TEST_KEY, &dir).unwrap();
        assert_eq!(p.signed, "pack test");
        assert_eq!((p.pack.spm_version.as_str(), p.pack.sentient_version.as_str(), p.pack.gateway_version.as_deref()), ("0.9.4", "4.4.0.4", Some("4.16.2")));
        assert_eq!(p.pack.min_os.windows_build, Some(19044));
        assert_eq!(p.pack.components.docker.as_deref(), Some("28.5.1"));
        assert_eq!(p.pack.files.len(), 5);
        assert_eq!(p.files(Kind::Deb).len(), 2);
        assert_eq!(p.image("quakestring/sentient:4.4.0.4").map(|f| f.path.as_str()), Some("images/sentient-4.4.0.4.tar.zst"));
        assert_eq!(p.payload_dir(), Some(dir.join("payload")));
        assert_eq!(p.pack.licence_build(), "4.4.0");
        assert_eq!(p.pack.check_title(), "Checking the offline pack: 5 files, 60 bytes");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_tampered_pack_json_is_refused() {
        let dir = pack_dir("tampered", true);
        std::fs::write(dir.join(PACK_FILE), PACK_JSON.replace("4.4.0.4", "4.4.0.5")).unwrap();
        let e = open_with_key(TEST_KEY, &dir).unwrap_err();
        assert!(matches!(e, Refusal::BadSignature(_)), "{e:?}");
        assert!(e.message().contains("signature is not valid"), "{}", e.message());
        assert!(e.message().ends_with("Nothing was changed."));
        // The real release key does not take the test signature either.
        let dir2 = pack_dir("otherkey", true);
        assert!(matches!(open(&dir2), Err(Refusal::BadSignature(_))));
        // A tampered trusted comment.
        std::fs::write(dir2.join(SIGNATURE_FILE), PACK_SIG.replace("pack test", "pack evil")).unwrap();
        assert!(matches!(open_with_key(TEST_KEY, &dir2), Err(Refusal::BadSignature(_))));
        for d in [dir, dir2] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn an_unsigned_pack_is_refused_and_a_folder_without_one_is_not_a_pack() {
        let dir = pack_dir("unsigned", false);
        let e = open_with_key(TEST_KEY, &dir).unwrap_err();
        assert_eq!(e, Refusal::Unsigned);
        assert!(e.message().contains("not signed"));
        std::fs::remove_file(dir.join(PACK_FILE)).unwrap();
        assert!(matches!(open_with_key(TEST_KEY, &dir), Err(Refusal::NotAPack(_))));
        // Garbage in place of a signature.
        let dir2 = pack_dir("garbage", true);
        std::fs::write(dir2.join(SIGNATURE_FILE), "garbage").unwrap();
        assert!(matches!(open_with_key(TEST_KEY, &dir2), Err(Refusal::BadSignature(_))));
        for d in [dir, dir2] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_newer_format_and_paths_outside_the_pack_are_refused() {
        assert_eq!(parse(r#"{"format":2,"anything":"else"}"#), Err(Refusal::NewerFormat(2)));
        let base = PACK_JSON.to_string();
        for bad in ["../evil.exe", "/etc/passwd", r"C:\Windows\evil.exe", r"payload\..\..\evil", ""] {
            let text = base.replace("payload/sentient-payload.tar", &bad.replace('\\', "\\\\"));
            assert!(matches!(parse(&text), Err(Refusal::BadPath(_))), "{bad}");
        }
        assert!(parse(&base).is_ok());
        assert!(matches!(parse("not json"), Err(Refusal::Unreadable(_))));
    }

    #[test]
    fn every_file_is_verified_with_progress() {
        let dir = pack_dir("verify", true);
        let p = open_with_key(TEST_KEY, &dir).unwrap();
        let seen = std::cell::RefCell::new(Vec::new());
        p.verify_files(&|c| seen.borrow_mut().push(c.clone()), &|| false).unwrap();
        let seen = seen.into_inner();
        let last = seen.last().unwrap();
        assert_eq!((last.done_files, last.files, last.done_bytes, last.bytes), (5, 5, 60, 60));
        assert_eq!(last.fraction(), 1.0);
        assert!(seen.iter().any(|c| c.file == "wsl/docker-debs/docker-ce.deb"));
        p.check_present().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_resized_or_changed_file_is_named() {
        let dir = pack_dir("damaged", true);
        let p = open_with_key(TEST_KEY, &dir).unwrap();
        let never = || false;
        // Changed, same size.
        std::fs::write(dir.join("wsl/docker-debs/docker-ce.deb"), "deb-docker-cX\n").unwrap();
        let e = p.verify_files(&|_| {}, &never).unwrap_err();
        assert_eq!(e, Refusal::Changed("wsl/docker-debs/docker-ce.deb".into()));
        assert!(e.message().contains("wsl/docker-debs/docker-ce.deb does not match its checksum"));
        // Another size.
        std::fs::write(dir.join("gateway/SentientGatewaySetup-4.16.2.exe"), "gateway, longer\n").unwrap();
        let e = p.verify_files(&|_| {}, &never).unwrap_err();
        assert_eq!(e, Refusal::WrongSize { file: "gateway/SentientGatewaySetup-4.16.2.exe".into(), expected: 8, found: 16 });
        assert!(p.check_present().is_err());
        // Missing.
        std::fs::remove_file(dir.join("payload/sentient-payload.tar")).unwrap();
        let e = p.verify_files(&|_| {}, &never).unwrap_err();
        assert_eq!(e, Refusal::Missing("payload/sentient-payload.tar".into()));
        assert!(e.message().contains("payload/sentient-payload.tar is missing"));
        // Cancelled.
        let dir2 = pack_dir("cancel", true);
        let p2 = open_with_key(TEST_KEY, &dir2).unwrap();
        assert_eq!(p2.verify_files(&|_| {}, &|| true), Err(Refusal::Cancelled));
        for d in [dir, dir2] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn the_target_is_windows_x64_of_a_new_enough_build() {
        let pack = parse(PACK_JSON).unwrap();
        assert_eq!(check_target(&pack, &windows(22631)), Ok(()));
        assert_eq!(check_target(&pack, &windows(19044)), Ok(()));
        let e = check_target(&pack, &windows(19041)).unwrap_err();
        assert_eq!(e, Refusal::OsTooOld { need: 19044, here: 19041 });
        assert!(e.message().contains("needs Windows 10 build 19044 or newer; this computer runs build 19041"));
        let linux = Host { os: "linux".into(), os_name: "Ubuntu 24.04.1 LTS".into(), arch: "x86_64".into(), windows_build: None };
        let e = check_target(&pack, &linux).unwrap_err();
        assert_eq!(e.message(), "This offline pack is for Windows x64; this computer runs Ubuntu 24.04.1 LTS. Download the pack for this computer. Nothing was changed.");
        let arm = Host { arch: "aarch64".into(), ..windows(26100) };
        let e = check_target(&pack, &arm).unwrap_err();
        assert!(e.message().starts_with("This offline pack is for Windows x64 (64-bit Intel or AMD); this computer has an ARM64 processor."), "{}", e.message());
        // Another target altogether.
        let other = Pack { target: "linux-arm64".into(), ..pack };
        assert!(check_target(&other, &windows(22631)).unwrap_err().message().contains("is for Linux ARM64; this computer runs Windows 11"));
    }

    #[test]
    fn the_part_another_account_reads_is_copied_with_the_signature() {
        let dir = pack_dir("copy", true);
        let p = open_with_key(TEST_KEY, &dir).unwrap();
        let to = dir.with_extension("part");
        let _ = std::fs::remove_dir_all(&to);
        let part = p.copy_part(&to, &|f| f.kind == Kind::Deb).unwrap();
        assert!(to.join("wsl/docker-debs/containerd.io.deb").is_file());
        assert!(!to.join("payload/sentient-payload.tar").exists());
        // It opens on its own, signature and all.
        let again = open_with_key(TEST_KEY, &to).unwrap();
        assert_eq!(again.pack, part.pack);
        assert!(again.recheck(again.files(Kind::Deb)[0]).is_ok());
        for d in [dir, to] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn sizes_read_as_a_file_manager_shows_them() {
        assert_eq!(size_text(3_200_000_000), "3.2 GB");
        assert_eq!(size_text(515_000_000), "515 MB");
        assert_eq!(size_text(8_400), "8 KB");
        let fake = debug_fake();
        assert!(fake.pack.check_title().starts_with("Checking the offline pack: 41 files, 3."), "{}", fake.pack.check_title());
        assert!(fake.has_docker());
    }
}
