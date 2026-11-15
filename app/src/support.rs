// Copyright © 2016-2026 The SENTIENT Authors

//! "Create a support file": one archive with what helps support find a
//! problem, and no passwords: the Platform Manager's activity log, its
//! preferences, the installation record (which holds no passwords),
//! versions, and the last lines of SENTIENT's log.

use std::path::{Path, PathBuf};

use crate::installation::InstallationRecord;

/// Where the file goes: the user's Downloads folder, else home.
fn target_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

fn add(tar: &mut tar::Builder<std::fs::File>, name: &str, bytes: &[u8]) -> Result<(), String> {
    let mut h = tar::Header::new_gnu();
    h.set_size(bytes.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(chrono::Utc::now().timestamp().max(0) as u64);
    h.set_cksum();
    tar.append_data(&mut h, name, bytes).map_err(|e| format!("write {name}: {e}"))
}

fn add_file(tar: &mut tar::Builder<std::fs::File>, name: &str, path: &Path) -> Result<(), String> {
    match std::fs::read(path) {
        Ok(bytes) => add(tar, name, &bytes),
        Err(_) => Ok(()),
    }
}

/// Write the support file. Blocking. Returns its path.
pub fn create() -> Result<PathBuf, String> {
    let path = target_dir().join(format!("sentient-support-{}.tar", chrono::Local::now().format("%Y-%m-%d_%H%M")));
    let file = std::fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut tar = tar::Builder::new(file);
    let record = InstallationRecord::load();
    let versions = format!(
        "Platform Manager {}\nSENTIENT {}\nOperating system {} {}\nCreated {}\n",
        crate::updates::manager_version(),
        record.as_ref().map(|r| r.sentient_version.clone()).unwrap_or_else(|| "not installed".into()),
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Local::now().to_rfc3339(),
    );
    add(&mut tar, "versions.txt", versions.as_bytes())?;
    add_file(&mut tar, "activity.log", &crate::jobs::activity_log())?;
    add_file(&mut tar, "activity.log.1", &crate::jobs::activity_log().with_extension("log.1"))?;
    add_file(&mut tar, "updates.json", &crate::updates::prefs_dir().join("updates.json"))?;
    if let Some(r) = &record {
        // The record holds no passwords (they live in the secret store).
        let json = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
        add(&mut tar, "installation.json", json.as_bytes())?;
        let logs = r.platform().logs(2000);
        add(&mut tar, "sentient.log", crate::ui::strip_ansi(&logs).as_bytes())?;
    }
    tar.finish().map_err(|e| format!("finish {}: {e}", path.display()))?;
    Ok(path)
}
