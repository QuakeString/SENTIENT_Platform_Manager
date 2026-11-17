// Copyright © 2016-2026 The SENTIENT Authors

//! A PostgreSQL data folder from an older major version, found before a
//! native Linux install, and the two ways forward: upgrade it to the
//! installed major (the data is kept), or keep it aside and start fresh
//! (moved, never deleted).
//!
//! Each distribution packages this differently:
//! - Arch: one data folder, `/var/lib/postgres/data`; the previous major's
//!   binaries come from `postgresql-old-upgrade` under `/opt/pgsql-NN`, and
//!   the upgrade is `pg_upgrade` into a fresh `data` with the old one moved
//!   to `olddata` (the Arch wiki's procedure);
//! - Fedora/RHEL: `/var/lib/pgsql/data`, upgraded by `postgresql-setup
//!   --upgrade` from the `postgresql-upgrade` package (the old data is kept
//!   as `data-old`);
//! - Debian/Ubuntu: clusters per major (`pg_lsclusters`), upgraded by
//!   `pg_upgradecluster`, which leaves the old cluster in place.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What to do with a data folder from another major, chosen before the
/// install. It only acts when the majors differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OldPgChoice {
    /// Not chosen: the install stops with exit 42 and says why.
    #[default]
    Ask,
    /// Upgrade it to the installed major; the old data is kept.
    Upgrade,
    /// Move it to `<data>.old-<date>` and start with an empty one.
    KeepAside,
}

/// Which packaging the machine uses for PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    Arch,
    Rhel,
    Debian,
}

impl Family {
    pub fn here() -> Option<Family> {
        let have = |b: &str| crate::sys::host_command("sh").arg("-c").arg(format!("command -v {b}")).output().is_ok_and(|o| o.status.success());
        if have("pacman") {
            Some(Family::Arch)
        } else if have("apt-get") {
            Some(Family::Debian)
        } else if have("dnf") {
            Some(Family::Rhel)
        } else {
            None
        }
    }

    /// The single data folder (Arch, Fedora); Debian has one per cluster.
    pub fn data_dir(self) -> &'static str {
        match self {
            Family::Arch => "/var/lib/postgres/data",
            Family::Rhel => "/var/lib/pgsql/data",
            Family::Debian => "/var/lib/postgresql",
        }
    }
}

/// An older-major data folder, as found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OldPgData {
    pub family: Family,
    pub dir: PathBuf,
    /// Its major; None when the folder could not be read without the
    /// administrator password (Arch and Fedora keep it 0700 postgres).
    pub have: Option<u32>,
    /// The major that is (or will be) installed; None when unknown.
    pub want: Option<u32>,
}

impl OldPgData {
    /// Known to be older (both majors read and different).
    pub fn is_older(&self) -> bool {
        matches!((self.have, self.want), (Some(h), Some(w)) if h < w)
    }

    /// "PostgreSQL 17 data is in /var/lib/postgres/data; PostgreSQL 18 is installed."
    pub fn describe(&self) -> String {
        let want = self.want.map(|w| format!("PostgreSQL {w}")).unwrap_or_else(|| "the PostgreSQL this install uses".into());
        match self.have {
            Some(h) => format!("PostgreSQL {h} data is in {}; {want} will be used.", self.dir.display()),
            None => format!("PostgreSQL data is already in {}. If it is from an older version than {want}, choose what to do with it.", self.dir.display()),
        }
    }
}

/// The leading major of a version text: "postgres (PostgreSQL) 18.7" → 18,
/// "18.7-1" → 18, "17+278" → 17.
pub fn major_of(text: &str) -> Option<u32> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    text[start..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn out(cmd: &str) -> Option<String> {
    let o = crate::sys::host_command("sh").arg("-c").arg(cmd).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// The major the install will use: the installed server, else what the
/// package manager would install.
pub fn wanted_major(family: Family) -> Option<u32> {
    if let Some(v) = out("postgres --version 2>/dev/null").and_then(|v| major_of(&v)) {
        return Some(v);
    }
    match family {
        Family::Arch => out("LC_ALL=C pacman -Si postgresql 2>/dev/null | sed -n 's/^Version *: *//p'").and_then(|v| major_of(&v)),
        Family::Rhel => out("LC_ALL=C dnf -q info postgresql-server 2>/dev/null | sed -n 's/^Version *: *//p' | tail -1").and_then(|v| major_of(&v)),
        Family::Debian => out("LC_ALL=C apt-cache policy postgresql 2>/dev/null | sed -n 's/^ *Candidate: *//p'").and_then(|v| major_of(&v)),
    }
}

/// Look without the administrator password. Arch and Fedora data folders
/// are 0700 postgres, so their major is only known when this runs as root
/// (or as postgres); [`probe_privileged`] reads it with the password.
pub fn find() -> Option<OldPgData> {
    let family = Family::here()?;
    find_in(family, Path::new(family.data_dir()), wanted_major(family))
}

/// [`find`] on a given folder (tests use a temporary one).
pub fn find_in(family: Family, dir: &Path, want: Option<u32>) -> Option<OldPgData> {
    match family {
        Family::Arch | Family::Rhel => {
            if !dir.exists() {
                return None;
            }
            let have = match std::fs::read_to_string(dir.join("PG_VERSION")) {
                Ok(t) => Some(major_of(&t)?),
                // Not readable: there is something, its version unknown.
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => None,
                // Readable and empty (no PG_VERSION): nothing to keep.
                Err(_) if std::fs::read_dir(dir).is_ok() => return None,
                Err(_) => None,
            };
            let found = OldPgData { family, dir: dir.to_path_buf(), have, want };
            // The same major is no problem; unknown is offered.
            (have.is_none() || found.is_older()).then_some(found)
        }
        Family::Debian => {
            // `pg_lsclusters` needs no password: the oldest cluster older
            // than the version installed.
            let list = out("pg_lsclusters -h 2>/dev/null")?;
            oldest_debian_cluster(&list, want).map(|(v, name)| OldPgData {
                family,
                dir: dir.join(v.to_string()).join(name),
                have: Some(v),
                want,
            })
        }
    }
}

/// From `pg_lsclusters -h` ("17 main 5432 online postgres /var/lib/… …"):
/// the oldest cluster below `want` (or below the newest listed).
pub fn oldest_debian_cluster(list: &str, want: Option<u32>) -> Option<(u32, String)> {
    let rows: Vec<(u32, String)> = list
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.parse().ok()?, w.next()?.to_string()))
        })
        .collect();
    let target = want.or_else(|| rows.iter().map(|r| r.0).max())?;
    rows.into_iter().filter(|(v, _)| *v < target).min_by_key(|(v, _)| *v)
}

/// Read the major of an unreadable data folder as root (one password prompt).
#[cfg(unix)]
pub fn probe_privileged(found: &OldPgData) -> Option<OldPgData> {
    let script = format!("cat {}/PG_VERSION", crate::exec::shq(&found.dir.display().to_string()));
    let (ok, stdout, _) = crate::exec::privileged_output(&script)?;
    let have = ok.then(|| major_of(&stdout)).flatten()?;
    Some(OldPgData { have: Some(have), ..found.clone() })
}

/// Windows has no such folder.
#[cfg(not(unix))]
pub fn probe_privileged(_: &OldPgData) -> Option<OldPgData> {
    None
}

/// The shell (bash, as root, `set -e`) that applies `choice` to the data
/// folder when it is from another major; nothing when it is the same or
/// there is none. Sets nothing else up: the install continues after it
/// (an empty data folder gets its `initdb` there).
///
/// Exit codes: 44 the upgrade is not possible here (the message says why),
/// 45 the upgrade itself failed (the old data is back where it was).
pub fn resolve_script(family: Family, choice: OldPgChoice) -> String {
    match (family, choice) {
        (_, OldPgChoice::Ask) => String::new(),
        (Family::Arch, OldPgChoice::Upgrade) => ARCH_UPGRADE.to_string(),
        (Family::Arch, OldPgChoice::KeepAside) => keep_aside("/var/lib/postgres/data"),
        (Family::Rhel, OldPgChoice::Upgrade) => RHEL_UPGRADE.to_string(),
        (Family::Rhel, OldPgChoice::KeepAside) => keep_aside("/var/lib/pgsql/data"),
        (Family::Debian, OldPgChoice::Upgrade) => DEBIAN_UPGRADE.to_string(),
        (Family::Debian, OldPgChoice::KeepAside) => DEBIAN_KEEP_ASIDE.to_string(),
    }
}

/// The packages `resolve_script` needs installed first.
pub fn extra_packages(family: Family, choice: OldPgChoice) -> &'static str {
    match (family, choice) {
        (Family::Arch, OldPgChoice::Upgrade) => " postgresql-old-upgrade",
        (Family::Rhel, OldPgChoice::Upgrade) => " postgresql-upgrade",
        _ => "",
    }
}

/// `MAJOR_OF_DATA <dir>` / `WANT_MAJOR`: helpers the scripts share.
const HELPERS: &str = "pg_have() { [ -f \"$1/PG_VERSION\" ] && cat \"$1/PG_VERSION\" || true; }\n\
pg_want() { postgres --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\\1/'; }\n";

fn keep_aside(data: &str) -> String {
    format!(
        "{HELPERS}DATA={data}\n\
         HAVE=$(pg_have \"$DATA\"); WANT=$(pg_want)\n\
         if [ -n \"$HAVE\" ] && [ -n \"$WANT\" ] && [ \"$HAVE\" != \"$WANT\" ]; then\n\
           systemctl stop postgresql 2>/dev/null || true\n\
           ASIDE=\"$DATA.old-$(date +%Y%m%d-%H%M%S)\"\n\
           mv \"$DATA\" \"$ASIDE\"\n\
           echo \">> The PostgreSQL $HAVE data is kept aside in $ASIDE (nothing was deleted)\"\n\
         fi\n"
    )
}

/// Arch: the wiki's pg_upgrade procedure, with the new cluster made like the
/// old one (encoding, locale, provider, checksums: pg_upgrade refuses any
/// difference). The old data stays in `olddata`; when the upgrade fails it
/// goes back to `data` and only the new, empty cluster is removed.
const ARCH_UPGRADE: &str = "pg_have() { [ -f \"$1/PG_VERSION\" ] && cat \"$1/PG_VERSION\" || true; }\n\
pg_want() { postgres --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\\1/'; }\n\
DATA=/var/lib/postgres/data\n\
HAVE=$(pg_have \"$DATA\"); WANT=$(pg_want)\n\
if [ -n \"$HAVE\" ] && [ -n \"$WANT\" ] && [ \"$HAVE\" != \"$WANT\" ]; then\n\
  OB=/opt/pgsql-$HAVE/bin\n\
  if [ ! -x \"$OB/pg_ctl\" ]; then\n\
    echo \"!! The PostgreSQL $HAVE data cannot be upgraded here: postgresql-old-upgrade provides PostgreSQL $(ls /opt 2>/dev/null | sed -n 's/^pgsql-//p' | tr '\\n' ' ')only.\"\n\
    echo '!! Upgrade it one major at a time with an older postgresql-old-upgrade, or keep it aside and start fresh.'\n\
    exit 44\n\
  fi\n\
  echo \">> Upgrading the PostgreSQL $HAVE data to PostgreSQL $WANT (the old data is kept)\"\n\
  systemctl stop postgresql 2>/dev/null || true\n\
  OLD=/var/lib/postgres/olddata\n\
  [ -e \"$OLD\" ] && OLD=\"$OLD-$(date +%Y%m%d-%H%M%S)\"\n\
  mv \"$DATA\" \"$OLD\"\n\
  SOCK=$(mktemp -d /tmp/spm-pgup.XXXXXX); chown postgres: \"$SOCK\"\n\
  undo() { echo \"!! The upgrade failed; the PostgreSQL $HAVE data is back in $DATA, unchanged.\"; rm -rf \"$DATA\"; mv \"$OLD\" \"$DATA\"; exit 45; }\n\
  CHK=$(runuser -u postgres -- \"$OB/pg_controldata\" \"$OLD\" | sed -n 's/^Data page checksum version: *//p')\n\
  runuser -u postgres -- \"$OB/pg_ctl\" -D \"$OLD\" -o \"-p 50432 -k $SOCK -c listen_addresses=''\" -l \"$SOCK/old.log\" -w start >/dev/null || undo\n\
  ROW=$(runuser -u postgres -- \"$OB/psql\" -h \"$SOCK\" -p 50432 -d template1 -qAt -F '|' -c \"select pg_encoding_to_char(encoding), datcollate, datctype, coalesce(to_jsonb(d)->>'datlocprovider','c'), coalesce(to_jsonb(d)->>'datlocale', to_jsonb(d)->>'daticulocale','') from pg_database d where datname='template1'\") || ROW=''\n\
  runuser -u postgres -- \"$OB/pg_ctl\" -D \"$OLD\" -w stop >/dev/null || undo\n\
  [ -n \"$ROW\" ] || undo\n\
  IFS='|' read -r ENC COLL CTYPE PROV ICU <<EOF\n\
$ROW\n\
EOF\n\
  ARGS=\"--encoding=$ENC --lc-collate=$COLL --lc-ctype=$CTYPE\"\n\
  [ \"$PROV\" = i ] && ARGS=\"$ARGS --locale-provider=icu --icu-locale=$ICU\"\n\
  [ \"$PROV\" = b ] && ARGS=\"$ARGS --locale-provider=builtin --builtin-locale=$ICU\"\n\
  [ \"$CHK\" = 0 ] && ARGS=\"$ARGS --no-data-checksums\"\n\
  install -d -o postgres -g postgres -m 700 \"$DATA\"\n\
  runuser -u postgres -- initdb -D \"$DATA\" $ARGS >/dev/null || undo\n\
  (cd \"$SOCK\" && runuser -u postgres -- pg_upgrade -b \"$OB\" -B /usr/bin -d \"$OLD\" -D \"$DATA\" -s \"$SOCK\" -p 50432 -P 50433) || undo\n\
  rm -rf \"$SOCK\"\n\
  echo \">> Upgraded to PostgreSQL $WANT; the PostgreSQL $HAVE data is kept in $OLD\"\n\
fi\n";

/// Fedora/RHEL: the distribution's own upgrade (it keeps `data-old`).
const RHEL_UPGRADE: &str = "pg_have() { [ -f \"$1/PG_VERSION\" ] && cat \"$1/PG_VERSION\" || true; }\n\
pg_want() { postgres --version 2>/dev/null | sed -E 's/[^0-9]*([0-9]+).*/\\1/'; }\n\
DATA=/var/lib/pgsql/data\n\
HAVE=$(pg_have \"$DATA\"); WANT=$(pg_want)\n\
if [ -n \"$HAVE\" ] && [ -n \"$WANT\" ] && [ \"$HAVE\" != \"$WANT\" ]; then\n\
  echo \">> Upgrading the PostgreSQL $HAVE data to PostgreSQL $WANT (the old data is kept in /var/lib/pgsql/data-old)\"\n\
  systemctl stop postgresql 2>/dev/null || true\n\
  postgresql-setup --upgrade || { echo '!! postgresql-setup --upgrade failed; its log is /var/lib/pgsql/upgrade_postgresql.log'; tail -n 30 /var/lib/pgsql/upgrade_postgresql.log 2>/dev/null || true; exit 45; }\n\
  echo \">> Upgraded to PostgreSQL $WANT\"\n\
fi\n";

/// Debian/Ubuntu: `pg_upgradecluster` (the old cluster stays, stopped, on
/// its own port). The package creates an empty `main` cluster of the new
/// major first; it is dropped only while it holds no database of its own.
const DEBIAN_UPGRADE: &str = "OLDV=$(pg_lsclusters -h 2>/dev/null | sort -n | awk 'NR==1{print $1\" \"$2}')\n\
NEWV=$(pg_lsclusters -h 2>/dev/null | sort -n | awk 'END{print $1}')\n\
set -- $OLDV\n\
if [ -n \"$1\" ] && [ -n \"$NEWV\" ] && [ \"$1\" != \"$NEWV\" ]; then\n\
  echo \">> Upgrading the PostgreSQL $1 cluster $2 to PostgreSQL $NEWV (the old cluster is kept)\"\n\
  if pg_lsclusters -h | awk -v v=\"$NEWV\" -v n=\"$2\" '$1==v && $2==n' | grep -q .; then\n\
    NEWPORT=$(pg_lsclusters -h | awk -v v=\"$NEWV\" -v n=\"$2\" '$1==v && $2==n{print $3}')\n\
    pg_ctlcluster \"$NEWV\" \"$2\" start 2>/dev/null || true\n\
    OWN=$(runuser -u postgres -- psql -p \"$NEWPORT\" -qAt -c \"select count(*) from pg_database where datname not in ('postgres','template0','template1')\" 2>/dev/null || echo 1)\n\
    if [ \"$OWN\" != 0 ]; then echo \"!! PostgreSQL $NEWV already has a cluster $2 with databases in it; not touching it.\"; exit 44; fi\n\
    pg_dropcluster --stop \"$NEWV\" \"$2\"\n\
  fi\n\
  pg_upgradecluster \"$1\" \"$2\" || exit 45\n\
  pg_ctlcluster \"$1\" \"$2\" stop 2>/dev/null || true\n\
  echo manual > \"/etc/postgresql/$1/$2/start.conf\"\n\
  echo \">> Upgraded; the PostgreSQL $1 cluster is kept, stopped\"\n\
fi\n";

/// Debian/Ubuntu: rename the old cluster aside (its folder moves with it)
/// and stop it from starting; nothing is deleted.
const DEBIAN_KEEP_ASIDE: &str = "OLDV=$(pg_lsclusters -h 2>/dev/null | sort -n | awk 'NR==1{print $1\" \"$2}')\n\
NEWV=$(pg_lsclusters -h 2>/dev/null | sort -n | awk 'END{print $1}')\n\
set -- $OLDV\n\
if [ -n \"$1\" ] && [ -n \"$NEWV\" ] && [ \"$1\" != \"$NEWV\" ]; then\n\
  ASIDE=\"$2-old-$(date +%Y%m%d-%H%M%S)\"\n\
  pg_ctlcluster \"$1\" \"$2\" stop 2>/dev/null || true\n\
  pg_renamecluster \"$1\" \"$2\" \"$ASIDE\"\n\
  echo manual > \"/etc/postgresql/$1/$ASIDE/start.conf\"\n\
  echo \">> The PostgreSQL $1 cluster is kept aside as $1/$ASIDE (stopped; nothing was deleted)\"\n\
fi\n";

/// What the fix buttons run on their own (as root): the packages, then
/// the choice.
pub fn fix_script(family: Family, choice: OldPgChoice) -> String {
    let pkgs = extra_packages(family, choice).trim();
    let install = match family {
        _ if pkgs.is_empty() => String::new(),
        Family::Arch => format!("pacman -S --noconfirm --needed {pkgs}\n"),
        Family::Rhel => format!("dnf install -y {pkgs}\n"),
        Family::Debian => String::new(),
    };
    format!("set -e\n{install}{}", resolve_script(family, choice))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majors_are_read_from_every_version_text() {
        assert_eq!(major_of("postgres (PostgreSQL) 18.7"), Some(18));
        assert_eq!(major_of("18.7-1"), Some(18));
        assert_eq!(major_of("17+278"), Some(17));
        assert_eq!(major_of("17\n"), Some(17));
        assert_eq!(major_of("none"), None);
    }

    #[test]
    fn debian_oldest_cluster_below_the_new_major() {
        let list = "15 main 5432 online postgres /var/lib/postgresql/15/main /var/log/postgresql/postgresql-15-main.log\n\
                    17 main 5433 online postgres /var/lib/postgresql/17/main /var/log/postgresql/postgresql-17-main.log\n";
        assert_eq!(oldest_debian_cluster(list, Some(17)), Some((15, "main".into())));
        assert_eq!(oldest_debian_cluster(list, None), Some((15, "main".into())));
        assert_eq!(oldest_debian_cluster("17 main 5432 online x y z\n", Some(17)), None);
    }

    #[test]
    fn a_readable_data_folder_is_offered_only_when_older() {
        let dir = std::env::temp_dir().join(format!("spm-oldpg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("PG_VERSION"), "17\n").unwrap();
        let f = find_in(Family::Arch, &dir, Some(18)).unwrap();
        assert_eq!((f.have, f.want, f.is_older()), (Some(17), Some(18), true));
        assert!(f.describe().contains("PostgreSQL 17 data is in"));
        assert_eq!(find_in(Family::Arch, &dir, Some(17)), None);
        std::fs::remove_file(dir.join("PG_VERSION")).unwrap();
        assert_eq!(find_in(Family::Arch, &dir, Some(18)), None, "empty folder: nothing to keep");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(find_in(Family::Arch, &dir, Some(18)), None);
    }

    // The scripts run on a Linux server: their text is checked everywhere,
    // and run through bash where there is one (cfg(unix)).
    #[test]
    fn every_script_is_valid_shell() {
        for fam in [Family::Arch, Family::Rhel, Family::Debian] {
            for ch in [OldPgChoice::Upgrade, OldPgChoice::KeepAside] {
                let s = fix_script(fam, ch);
                assert!(!s.trim().is_empty(), "{fam:?} {ch:?}");
                if ch == OldPgChoice::KeepAside {
                    assert!(!s.contains("rm -rf \"$DATA\"") && !s.contains("rm -rf $DATA"), "{fam:?} keep-aside deletes the data:\n{s}");
                }
                #[cfg(unix)]
                let ok = std::process::Command::new("bash").arg("-n").arg("-c").arg(&s).status().unwrap();
                #[cfg(unix)]
                assert!(ok.success(), "{fam:?} {ch:?}:\n{s}");
            }
        }
        assert_eq!(resolve_script(Family::Arch, OldPgChoice::Ask), "");
    }

    #[test]
    fn keep_aside_moves_and_never_deletes() {
        let text = keep_aside("/var/lib/postgres/data");
        assert!(text.contains("mv \"$DATA\" \"$ASIDE\""));
        assert!(!text.contains("rm "), "{text}");
        // Run it for real where there is a bash (the server's shell).
        #[cfg(unix)]
        keep_aside_runs();
    }

    #[cfg(unix)]
    fn keep_aside_runs() {
        // Run it for real on a temporary folder (systemctl absent or harmless).
        let base = std::env::temp_dir().join(format!("spm-aside-{}", std::process::id()));
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("PG_VERSION"), "17\n").unwrap();
        std::fs::write(data.join("keep.me"), "rows").unwrap();
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("postgres"), "#!/bin/sh\necho 'postgres (PostgreSQL) 18.7'\n").unwrap();
        std::fs::write(bin.join("systemctl"), "#!/bin/sh\nexit 0\n").unwrap();
        std::process::Command::new("chmod").args(["+x", bin.join("postgres").to_str().unwrap(), bin.join("systemctl").to_str().unwrap()]).status().unwrap();
        let script = format!("set -e\nPATH={}:$PATH\n{}", bin.display(), keep_aside(&data.display().to_string()));
        let o = std::process::Command::new("bash").arg("-c").arg(&script).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(!data.exists());
        let aside: Vec<_> = std::fs::read_dir(&base).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("data.old-")).collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(aside[0].path().join("keep.me")).unwrap(), "rows");
        std::fs::remove_dir_all(&base).ok();
    }
}
