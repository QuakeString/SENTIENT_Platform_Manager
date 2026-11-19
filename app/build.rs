// Copyright © 2016-2026 The SENTIENT Authors

//! Windows resources: the icon and version information (Explorer's Details
//! tab shows it, and comparing FileVersion with Cargo.toml is how a stale
//! build is caught). No manifest: GPUI always embeds its own (asInvoker,
//! per-monitor DPI), so elevation is done at startup instead (src/elevate.rs).

fn main() {
    embed_agents();
    bundle_locales();
    build_commit();
    build_date();
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/spm-app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    // Icon 1 is the Setup icon: Explorer shows it for the downloaded file,
    // and the window takes it at first. Icon 2 is the Platform Manager's:
    // its window switches to it (crate::win_icon) and its shortcuts use it.
    res.set_icon("assets/icon.ico")
        .set_icon_with_id("assets/spm-app.ico", "2")
        .set("ProductName", "SENTIENT Platform Manager")
        .set("FileDescription", "SENTIENT Setup and Platform Manager")
        .set("CompanyName", "INVENIA SYSTEMS")
        .set("LegalCopyright", "Copyright © 2016-2026 The SENTIENT Authors");
    res.compile().expect("compile the Windows resources");
}

/// The server agents this build carries (crate::remote): SPM_LINUX_AGENT
/// (x86_64), SPM_LINUX_AARCH64_AGENT and SPM_WINDOWS_AGENT name the built
/// agent files (`--no-default-features`; the Linux ones static, musl). A
/// build without one downloads the agent published with its version
/// instead, which only a published release has.
fn embed_agents() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("agents.rs");
    let mut code = String::new();
    for (var, name) in [("SPM_LINUX_AGENT", "LINUX"), ("SPM_LINUX_AARCH64_AGENT", "LINUX_AARCH64"), ("SPM_WINDOWS_AGENT", "WINDOWS")] {
        println!("cargo:rerun-if-env-changed={var}");
        match std::env::var(var).ok().filter(|p| std::path::Path::new(p).is_file()) {
            Some(path) => {
                println!("cargo:rerun-if-changed={path}");
                let path = std::fs::canonicalize(&path).unwrap();
                code.push_str(&format!("pub const {name}: Option<&[u8]> = Some(include_bytes!({:?}));\n", path.display().to_string()));
            }
            None => code.push_str(&format!("pub const {name}: Option<&[u8]> = None;\n")),
        }
    }
    std::fs::write(out, code).unwrap();
}

/// The commit this build is from (SPM_BUILD_COMMIT): shown beside the
/// version so two builds of one version are never confused. SPM_COMMIT
/// overrides it (package builds from an exported tree have no .git).
fn build_commit() {
    println!("cargo:rerun-if-env-changed=SPM_COMMIT");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    // Every commit appends to the HEAD log: a new commit rebuilds the text.
    println!("cargo:rerun-if-changed=../.git/logs/HEAD");
    let commit = std::env::var("SPM_COMMIT").ok().filter(|c| !c.is_empty()).or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    println!("cargo:rustc-env=SPM_BUILD_COMMIT={}", commit.unwrap_or_else(|| "unknown".into()));
}

/// The day this build was made (SPM_BUILD_DATE, UTC, YYYY-MM-DD), for
/// About and the support file. SPM_BUILD_DATE overrides it, else
/// SOURCE_DATE_EPOCH (reproducible builds), else today. "unknown" when the
/// clock cannot be read. It is set when this script runs, which is on a new
/// commit (see build_commit) or a clean build.
fn build_date() {
    println!("cargo:rerun-if-env-changed=SPM_BUILD_DATE");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let date = std::env::var("SPM_BUILD_DATE").ok().filter(|d| !d.is_empty()).or_else(|| {
        let secs = match std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|s| s.trim().parse::<u64>().ok()) {
            Some(s) => s,
            None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs(),
        };
        Some(civil_date(secs / 86_400))
    });
    println!("cargo:rustc-env=SPM_BUILD_DATE={}", date.unwrap_or_else(|| "unknown".into()));
}

/// Days since 1970-01-01 as YYYY-MM-DD (Howard Hinnant's civil_from_days).
fn civil_date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The translations (i18n/<code>.json), compressed into the program;
/// crate::i18n reads one when the language is chosen. en.json is the list of
/// texts, not a translation, and stays out.
fn bundle_locales() {
    use std::io::Write as _;
    println!("cargo:rerun-if-changed=i18n");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut code = String::from("pub static LOCALES: &[(&str, &[u8])] = &[\n");
    if std::env::var_os("CARGO_FEATURE_GUI").is_some() {
        let mut files: Vec<_> = std::fs::read_dir("i18n").map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        files.sort();
        for p in files {
            let Some(name) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            if p.extension().is_none_or(|e| e != "json") || name == "en" {
                continue;
            }
            let raw = std::fs::read(&p).unwrap();
            let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
            z.write_all(&raw).unwrap();
            let packed = out.join(format!("i18n-{name}.deflate"));
            std::fs::write(&packed, z.finish().unwrap()).unwrap();
            code.push_str(&format!("    ({name:?}, include_bytes!({:?})),\n", packed.display().to_string()));
        }
    }
    code.push_str("];\n");
    std::fs::write(out.join("i18n_bundle.rs"), code).unwrap();
}
