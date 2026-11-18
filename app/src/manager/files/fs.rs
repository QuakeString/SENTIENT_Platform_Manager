// Copyright © 2016-2026 The SENTIENT Authors

//! The Files page's two sides as one interface: this computer (std::fs) and
//! a server over SFTP (crate::sftp), as its account or as administrator.
//! Paths are strings: this computer's own form, the server's with `/`.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::remote::Instance;
use crate::sftp::{self, Sftp};

/// One file or folder in a listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Seconds since 1970.
    #[serde(default)]
    pub mtime: Option<i64>,
    /// The owner's name (the server's side).
    #[serde(default)]
    pub owner: String,
}

/// A folder as a pane shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub free: Option<u64>,
    /// The account may change things here (false: C6, read only).
    pub writable: bool,
    /// Who owns the folder ("root").
    #[serde(default)]
    pub owner: String,
}

/// A shortcut chip: label, icon name, path.
pub type Shortcut = (String, &'static str, String);

/// Where a pane's files are.
#[derive(Clone)]
pub enum Place {
    Local,
    Remote(Instance),
}

impl Place {
    pub fn is_remote(&self) -> bool {
        matches!(self, Place::Remote(_))
    }
    pub fn key(&self) -> String {
        match self {
            Place::Local => "local".into(),
            Place::Remote(i) => i.id.clone(),
        }
    }
    /// Joined with this side's separator.
    pub fn join(&self, dir: &str, name: &str) -> String {
        match self {
            Place::Local => Path::new(dir).join(name).to_string_lossy().to_string(),
            Place::Remote(_) => sftp::join(dir, name),
        }
    }
    pub fn parent(&self, path: &str) -> Option<String> {
        match self {
            Place::Local => Path::new(path).parent().map(|p| p.to_string_lossy().to_string()),
            Place::Remote(_) => sftp::parent(path),
        }
    }
    /// The path's parts for the bar (C1: "home › ops › exports"), each with
    /// the path it opens.
    pub fn crumbs(&self, path: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut p = Some(path.to_string());
        while let Some(cur) = p {
            let name = match self {
                Place::Local => Path::new(&cur).file_name().map(|n| n.to_string_lossy().to_string()),
                Place::Remote(_) => cur.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).map(String::from),
            };
            match name {
                Some(n) => out.push((n, cur.clone())),
                None => {
                    // The root: "/" (or "C:\" on Windows) stays out of the bar
                    // unless it is all there is.
                    if out.is_empty() {
                        out.push((cur.clone(), cur.clone()));
                    }
                    break;
                }
            }
            p = self.parent(&cur);
        }
        out.reverse();
        out
    }
}

pub trait Fs: Send {
    fn list(&mut self, path: &str) -> Result<Listing, String>;
    /// (is a folder, size), or None when nothing is there.
    fn meta(&mut self, path: &str) -> Result<Option<(bool, u64, Option<i64>)>, String>;
    fn mkdir(&mut self, path: &str) -> Result<(), String>;
    /// A file, or a folder with everything in it.
    fn remove_tree(&mut self, path: &str) -> Result<(), String>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), String>;
    /// `size` bytes from `offset`, in order, to `sink` (false stops).
    fn read(&mut self, path: &str, offset: u64, size: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), String>;
    /// Write from `offset` (0: a new file) what `next` gives until None.
    fn write(&mut self, path: &str, offset: u64, next: &mut dyn FnMut() -> Result<Option<Vec<u8>>, String>) -> Result<(), String>;
    /// Where it starts (home).
    fn home(&mut self) -> String;
    fn shortcuts(&mut self) -> Vec<Shortcut>;
    /// The connection broke (a copy reconnects and resumes).
    fn broken(&self, _err: &str) -> bool {
        false
    }
    /// Round trips, bytes in, bytes out over the link so far (a server).
    fn counters(&self) -> [u64; 3] {
        [0; 3]
    }
}

/// ssh connections the Files page has opened (its timing marks).
pub static CONNECTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// -- This computer -----------------------------------------------------------

pub struct LocalFs;

#[cfg(unix)]
fn ids() -> &'static (u32, Vec<u32>) {
    static IDS: std::sync::OnceLock<(u32, Vec<u32>)> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let run = |a: &str| std::process::Command::new("id").arg(a).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let uid = run("-u").trim().parse().unwrap_or(u32::MAX);
        let gids = run("-G").split_whitespace().filter_map(|g| g.parse().ok()).collect();
        (uid, gids)
    })
}

/// May the account write in a folder with these owner, group and mode.
pub fn may_write(uid: u32, gids: &[u32], owner: u32, group: u32, mode: u32) -> bool {
    uid == 0 || (owner == uid && mode & 0o200 != 0) || (gids.contains(&group) && mode & 0o020 != 0) || mode & 0o002 != 0
}

#[cfg(unix)]
fn owner_name(uid: u32) -> String {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<u32, String>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            std::fs::read_to_string("/etc/passwd")
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split(':').collect();
                    Some((f.get(2)?.parse().ok()?, f.first()?.to_string()))
                })
                .collect()
        })
        .get(&uid)
        .cloned()
        .unwrap_or_else(|| uid.to_string())
}

fn mtime(m: &std::fs::Metadata) -> Option<i64> {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64)
}

impl Fs for LocalFs {
    fn list(&mut self, path: &str) -> Result<Listing, String> {
        let rd = std::fs::read_dir(path).map_err(|e| local_err(path, &e))?;
        let mut entries = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(m) = std::fs::metadata(e.path()) else { continue };
            entries.push(Entry { name, dir: m.is_dir(), size: if m.is_dir() { 0 } else { m.len() }, mtime: mtime(&m), owner: String::new() });
        }
        let meta = std::fs::metadata(path).map_err(|e| local_err(path, &e))?;
        #[cfg(unix)]
        let (writable, owner) = {
            use std::os::unix::fs::MetadataExt;
            let (uid, gids) = ids();
            (may_write(*uid, gids, meta.uid(), meta.gid(), meta.mode()), owner_name(meta.uid()))
        };
        #[cfg(not(unix))]
        let (writable, owner) = (!meta.permissions().readonly(), String::new());
        // No free space: the pane shows it for a server only, and listing the
        // disks took tens of milliseconds a folder (a network mount holds it).
        Ok(Listing { path: path.to_string(), entries, free: None, writable, owner })
    }

    fn meta(&mut self, path: &str) -> Result<Option<(bool, u64, Option<i64>)>, String> {
        match std::fs::metadata(path) {
            Ok(m) => Ok(Some((m.is_dir(), m.len(), mtime(&m)))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(local_err(path, &e)),
        }
    }

    fn mkdir(&mut self, path: &str) -> Result<(), String> {
        std::fs::create_dir(path).map_err(|e| local_err(path, &e))
    }

    fn remove_tree(&mut self, path: &str) -> Result<(), String> {
        let m = std::fs::symlink_metadata(path).map_err(|e| local_err(path, &e))?;
        if m.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }.map_err(|e| local_err(path, &e))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        std::fs::rename(from, to).map_err(|e| local_err(from, &e))
    }

    fn read(&mut self, path: &str, offset: u64, size: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), String> {
        let mut f = std::fs::File::open(path).map_err(|e| local_err(path, &e))?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| local_err(path, &e))?;
        let mut left = size.saturating_sub(offset);
        let mut buf = vec![0u8; sftp::CHUNK];
        while left > 0 {
            let n = f.read(&mut buf[..(left.min(sftp::CHUNK as u64) as usize)]).map_err(|e| local_err(path, &e))?;
            if n == 0 {
                break;
            }
            left -= n as u64;
            if !sink(&buf[..n]) {
                break;
            }
        }
        Ok(())
    }

    fn write(&mut self, path: &str, offset: u64, next: &mut dyn FnMut() -> Result<Option<Vec<u8>>, String>) -> Result<(), String> {
        let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(offset == 0).open(path).map_err(|e| local_err(path, &e))?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| local_err(path, &e))?;
        while let Some(data) = next()? {
            f.write_all(&data).map_err(|e| local_err(path, &e))?;
        }
        f.flush().map_err(|e| local_err(path, &e))
    }

    fn home(&mut self) -> String {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| h.to_string_lossy().to_string()).unwrap_or_else(|| "/".into())
    }

    fn shortcuts(&mut self) -> Vec<Shortcut> {
        let home = self.home();
        let mut out = vec![("Home".to_string(), "house", home.clone())];
        let dl = Path::new(&home).join("Downloads");
        if dl.is_dir() {
            out.push(("Downloads".into(), "download", dl.to_string_lossy().to_string()));
        }
        for d in sysinfo::Disks::new_with_refreshed_list().list() {
            let m = d.mount_point().to_string_lossy().to_string();
            if d.is_removable() || m.starts_with("/media/") || m.starts_with("/run/media/") {
                if !out.iter().any(|(_, _, p)| *p == m) {
                    out.push(("USB disk".into(), "hard-drive", m));
                }
            }
        }
        out
    }
}

fn local_err(path: &str, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("{path} is not there."),
        std::io::ErrorKind::PermissionDenied => format!("Permission denied: {path}."),
        std::io::ErrorKind::AlreadyExists => format!("{path} is already there."),
        _ => format!("{path}: {e}"),
    }
}

// -- A server over SFTP --------------------------------------------------------

/// What one shell call finds out when a pane connects: the account's ids,
/// its home, and where SENTIENT, its backups and logs are.
#[derive(Debug, Clone, Default)]
struct Probe {
    uid: u32,
    gids: Vec<u32>,
    home: String,
    sentient: Option<String>,
    backups: Option<String>,
    logs: Option<String>,
}

const PROBE: &str = r#"echo "UID $(id -u)"; echo "GIDS $(id -G)"; echo "HOME $HOME"; S=$(sed -n 's/.*"install_dir": *"\([^"]*\)".*/\1/p' /var/lib/sentient-platform-manager/installation.json 2>/dev/null | head -1); [ -n "$S" ] && [ -d "$S" ] && echo "SENTIENT $S"; B=$(sed -n 's/.*"folder": *"\([^"]*\)".*/\1/p' /var/lib/sentient-platform-manager/backup.json 2>/dev/null | head -1); [ -z "$B" ] && B=/var/backups/sentient; [ -d "$B" ] && echo "BACKUPS $B"; for d in "$S/logs" /var/log/sentient; do if [ -n "$d" ] && [ -d "$d" ]; then echo "LOGS $d"; break; fi; done; true"#;

fn probe(inst: &Instance) -> Probe {
    let out = crate::remote::run(inst, PROBE, None).unwrap_or_default();
    let mut p = Probe { uid: u32::MAX, ..Default::default() };
    for l in out.lines() {
        let (k, v) = l.split_once(' ').unwrap_or((l, ""));
        match k {
            "UID" => p.uid = v.trim().parse().unwrap_or(u32::MAX),
            "GIDS" => p.gids = v.split_whitespace().filter_map(|g| g.parse().ok()).collect(),
            "HOME" => p.home = v.trim().to_string(),
            "SENTIENT" => p.sentient = Some(v.trim().to_string()),
            "BACKUPS" => p.backups = Some(v.trim().to_string()),
            "LOGS" => p.logs = Some(v.trim().to_string()),
            _ => {}
        }
    }
    p
}

/// Whether the account may write in a server's folder, and the folder's
/// owner to name. A Windows server's OpenSSH reports POSIX ids it makes up
/// (uid 0, no write bits) even for the account's own folders: the read-only
/// banner said "belongs to root" in its Downloads. There Windows decides on
/// each write itself (a refused one shows its error) and no owner is named.
fn folder_access(windows: bool, here: &crate::sftp::Attrs, owner: Option<String>, uid: u32, gids: &[u32]) -> (bool, String) {
    if windows {
        return (true, String::new());
    }
    let owner = owner.filter(|o| !o.is_empty()).unwrap_or_else(|| if here.uid == Some(0) { "root".into() } else { String::new() });
    let writable = match (here.uid, here.gid, here.perm) {
        (Some(o), Some(g), Some(m)) => may_write(uid, gids, o, g, m),
        _ => true,
    };
    (writable, owner)
}

pub struct RemoteFs {
    s: Sftp,
    p: Probe,
    inst: Instance,
    admin: bool,
}

impl RemoteFs {
    /// Connect as the account, or as administrator (sudo; its password must
    /// be kept already, crate::manager::remote_pages::ask_sudo_password).
    pub fn connect(inst: &Instance, admin: bool) -> Result<RemoteFs, String> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut p = if crate::remote::is_windows(inst) {
            Probe { uid: u32::MAX, ..Default::default() }
        } else {
            CONNECTIONS.fetch_add(1, Relaxed);
            probe(inst)
        };
        CONNECTIONS.fetch_add(1, Relaxed);
        let (mut cmd, pw) = crate::remote::sftp_command(inst, admin);
        let child = cmd.spawn().map_err(|e| format!("start ssh: {e}"))?;
        let mut s = Sftp::start(child, pw.as_deref())?;
        if p.home.is_empty() {
            p.home = s.realpath(".").unwrap_or_else(|_| "/".into());
        }
        if admin {
            p.uid = 0;
        }
        Ok(RemoteFs { s, p, inst: inst.clone(), admin })
    }
}

impl Fs for RemoteFs {
    fn list(&mut self, path: &str) -> Result<Listing, String> {
        // One pipelined read (crate::sftp::Sftp::list_dir): the folder's
        // attributes, entries with their owners, links followed, free space.
        let d = self.s.list_dir(path)?;
        let here = d.attrs;
        // A Windows server's OpenSSH reports POSIX ids it makes up (uid 0,
        // no write bits) for folders the account owns: the read-only banner
        // said "belongs to root" in the account's own Downloads. Windows
        // decides on each write itself (a refused one shows its error).
        let windows = crate::remote::is_windows(&self.inst);
        let (writable, owner) = folder_access(windows, &here, d.owner, self.p.uid, &self.p.gids);
        let entries = d
            .entries
            .into_iter()
            .map(|e| Entry { dir: e.attrs.is_dir(), size: if e.attrs.is_dir() { 0 } else { e.attrs.size.unwrap_or(0) }, mtime: e.attrs.mtime.map(i64::from), owner: if windows { String::new() } else { e.owner }, name: e.name })
            .collect();
        Ok(Listing { path: path.to_string(), entries, free: d.free, writable, owner })
    }

    fn meta(&mut self, path: &str) -> Result<Option<(bool, u64, Option<i64>)>, String> {
        match self.s.stat(path) {
            Ok(a) => Ok(Some((a.is_dir(), a.size.unwrap_or(0), a.mtime.map(i64::from)))),
            Err(e) if e.code == 2 => Ok(None),
            Err(e) => Err(e.message),
        }
    }

    fn mkdir(&mut self, path: &str) -> Result<(), String> {
        Ok(self.s.mkdir(path)?)
    }

    fn remove_tree(&mut self, path: &str) -> Result<(), String> {
        let a = self.s.lstat(path)?;
        if !a.is_dir() {
            return Ok(self.s.remove(path)?);
        }
        // A folder: one command on the server, not file by file over SFTP
        // (a big tree took minutes); as administrator when the pane is.
        crate::remote::remove_tree(&self.inst, path, self.admin)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        Ok(self.s.rename(from, to)?)
    }

    fn read(&mut self, path: &str, offset: u64, size: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), String> {
        let h = self.s.open(path, sftp::O_READ)?;
        let r = self.s.read_from(&h, offset, size, sink, path);
        let _ = self.s.close_handle(&h);
        Ok(r?)
    }

    fn write(&mut self, path: &str, offset: u64, next: &mut dyn FnMut() -> Result<Option<Vec<u8>>, String>) -> Result<(), String> {
        let flags = sftp::O_WRITE | sftp::O_CREAT | if offset == 0 { sftp::O_TRUNC } else { 0 };
        let h = self.s.open(path, flags)?;
        let r = self.s.write_from(&h, offset, next, path);
        let c = self.s.close_handle(&h);
        r?;
        Ok(c?)
    }

    fn home(&mut self) -> String {
        self.p.home.clone()
    }

    fn shortcuts(&mut self) -> Vec<Shortcut> {
        let mut out = Vec::new();
        if let Some(s) = &self.p.sentient {
            out.push(("SENTIENT folder".to_string(), "server", s.clone()));
        }
        if let Some(b) = &self.p.backups {
            out.push(("Backups".into(), "archive", b.clone()));
        }
        if let Some(l) = &self.p.logs {
            out.push(("Logs".into(), "scroll-text", l.clone()));
        }
        out.push(("Home".into(), "house", self.p.home.clone()));
        out
    }

    fn broken(&self, err: &str) -> bool {
        err.starts_with("The connection to the server broke")
    }

    fn counters(&self) -> [u64; 3] {
        self.s.counters()
    }
}

// -- Screens: canned folders ---------------------------------------------------

/// SPM_DEBUG_FILES names a JSON file: {"local:<path>": Listing,
/// "remote:<path>": Listing, "local:home": "<path>", "remote:home": ...,
/// "local:shortcuts": [[label, icon, path]], ...}. Nothing changes.
#[cfg(debug_assertions)]
pub struct CannedFs {
    side: &'static str,
    all: serde_json::Value,
}

#[cfg(debug_assertions)]
impl CannedFs {
    pub fn load(remote: bool) -> Option<CannedFs> {
        let file = std::env::var_os("SPM_DEBUG_FILES")?;
        let all = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
        Some(CannedFs { side: if remote { "remote" } else { "local" }, all })
    }
    fn get(&self, k: &str) -> Option<&serde_json::Value> {
        self.all.get(format!("{}:{k}", self.side))
    }
}

#[cfg(debug_assertions)]
impl Fs for CannedFs {
    fn list(&mut self, path: &str) -> Result<Listing, String> {
        if let Some(secs) = std::env::var("SPM_DEBUG_REMOTE_SLOW").ok().and_then(|s| s.parse::<u64>().ok()) {
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
        self.get(path).cloned().and_then(|v| serde_json::from_value(v).ok()).ok_or_else(|| format!("{path} is not there."))
    }
    fn meta(&mut self, path: &str) -> Result<Option<(bool, u64, Option<i64>)>, String> {
        let up = PathBuf::from(path);
        let (dir, name) = (up.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(), up.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        Ok(self.list(&dir).ok().and_then(|l| l.entries.into_iter().find(|e| e.name == name)).map(|e| (e.dir, e.size, e.mtime)))
    }
    fn mkdir(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn remove_tree(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn rename(&mut self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn read(&mut self, _: &str, offset: u64, size: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), String> {
        let mut left = size.saturating_sub(offset);
        let piece = vec![0u8; 64 * 1024];
        while left > 0 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let n = left.min(piece.len() as u64) as usize;
            left -= n as u64;
            if !sink(&piece[..n]) {
                break;
            }
        }
        Ok(())
    }
    fn write(&mut self, _: &str, _: u64, next: &mut dyn FnMut() -> Result<Option<Vec<u8>>, String>) -> Result<(), String> {
        while next()?.is_some() {}
        Ok(())
    }
    fn home(&mut self) -> String {
        self.get("home").and_then(|v| v.as_str()).unwrap_or("/").to_string()
    }
    fn shortcuts(&mut self) -> Vec<Shortcut> {
        let icon = |i: &str| -> &'static str {
            match i {
                "house" => "house",
                "download" => "download",
                "server" => "server",
                "archive" => "archive",
                "scroll-text" => "scroll-text",
                _ => "hard-drive",
            }
        };
        self.get("shortcuts")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|s| Some((s.get(0)?.as_str()?.to_string(), icon(s.get(1)?.as_str()?), s.get(2)?.as_str()?.to_string()))).collect())
            .unwrap_or_default()
    }
}

/// Open a side: canned in screens, else this computer or the server.
pub fn open(place: &Place, admin: bool) -> Result<Box<dyn Fs>, String> {
    #[cfg(debug_assertions)]
    if let Some(c) = CannedFs::load(place.is_remote()) {
        return Ok(Box::new(c));
    }
    match place {
        Place::Local => Ok(Box::new(LocalFs)),
        Place::Remote(inst) => Ok(Box::new(RemoteFs::connect(inst, admin)?)),
    }
}

#[cfg(test)]
mod tests {

    /// Windows OpenSSH's made-up ids (uid 0, gid 0, no write bits) never
    /// make the account's own folder read-only or "root's"; on Linux the
    /// same attributes do.
    #[test]
    fn a_windows_folder_is_never_roots_or_read_only_from_made_up_ids() {
        let here = crate::sftp::Attrs { uid: Some(0), gid: Some(0), perm: Some(0o40555), ..Default::default() };
        assert_eq!(super::folder_access(true, &here, None, u32::MAX, &[]), (true, String::new()));
        assert_eq!(super::folder_access(false, &here, None, 1000, &[1000]), (false, "root".to_string()));
        let mine = crate::sftp::Attrs { uid: Some(1000), gid: Some(1000), perm: Some(0o40755), ..Default::default() };
        assert_eq!(super::folder_access(false, &mine, Some("ops".into()), 1000, &[1000]), (true, "ops".to_string()));
    }
    use super::*;

    #[test]
    fn write_rule() {
        assert!(may_write(0, &[], 5, 5, 0o500));
        assert!(may_write(1000, &[1000], 1000, 1000, 0o755));
        assert!(!may_write(1000, &[1000], 0, 0, 0o755));
        assert!(may_write(1000, &[27], 0, 27, 0o775));
        assert!(may_write(1000, &[], 0, 0, 0o777));
    }

    #[test]
    fn crumbs() {
        let p = Place::Remote(serde_json::from_str(r##"{"id":"t","name":"t","color":"#000","host":"h","user":"u"}"##).unwrap());
        let c = p.crumbs("/opt/sentient");
        assert_eq!(c, vec![("opt".to_string(), "/opt".to_string()), ("sentient".to_string(), "/opt/sentient".to_string())]);
        assert_eq!(p.crumbs("/"), vec![("/".to_string(), "/".to_string())]);
    }
}
