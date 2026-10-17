// Copyright © 2016-2026 The SENTIENT Authors

//! Windows resources: the icon and version information (Explorer's Details
//! tab shows it, and comparing FileVersion with Cargo.toml is how a stale
//! build is caught). No manifest: GPUI always embeds its own (asInvoker,
//! per-monitor DPI), so elevation is done at startup instead (src/elevate.rs).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "SENTIENT Platform Manager")
        .set("FileDescription", "SENTIENT Setup and Platform Manager")
        .set("CompanyName", "INVENIA SYSTEMS")
        .set("LegalCopyright", "Copyright © 2016-2026 The SENTIENT Authors");
    res.compile().expect("compile the Windows resources");
}
