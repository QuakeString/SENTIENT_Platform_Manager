// Copyright © 2016-2026 The SENTIENT Authors

//! Windows resources: the icon and version information (Explorer's Details
//! tab shows it, and comparing FileVersion with Cargo.toml is how a stale
//! build is caught). No manifest: GPUI always embeds its own (asInvoker,
//! per-monitor DPI), so elevation is done at startup instead (src/elevate.rs).

fn main() {
    embed_agents();
    build_commit();
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

/// The server agents this build carries (crate::remote): SPM_LINUX_AGENT and
/// SPM_WINDOWS_AGENT name the built agent files (`--no-default-features`;
/// the Linux one static, musl). Without them the program downloads the
/// agent published with its version instead.
fn embed_agents() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("agents.rs");
    let mut code = String::new();
    for (var, name) in [("SPM_LINUX_AGENT", "LINUX"), ("SPM_WINDOWS_AGENT", "WINDOWS")] {
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
