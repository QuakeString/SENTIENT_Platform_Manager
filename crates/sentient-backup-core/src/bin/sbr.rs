//! `sbr` — SENTIENT Backup & Restore CLI. A headless front-end over
//! `sentient-backup-core`.
//!
//!   sbr categories                       # the static component model
//!   sbr inspect  [conn]                  # per-component sizes on a live DB
//!   sbr backup   [conn] -o file [--no-telemetry]
//!   sbr restore  [conn] -i file [--allow-nonempty]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, human_bytes, ConnConfig, DbInspector};
use sentient_backup_core::files;
use sentient_backup_core::progress::{Progress, ProgressFn};
use sentient_backup_core::restore::{self, RestoreOptions};

#[derive(Parser)]
#[command(name = "sbr", version, about = "SENTIENT Backup & Restore (CLI)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the static backup-component catalog.
    Categories,
    /// Connect and report the size of each component.
    Inspect(ConnArgs),
    /// Back up a SENTIENT database to a .sentient-backup archive.
    Backup(BackupArgs),
    /// Restore a .sentient-backup archive into an (empty) database.
    Restore(RestoreArgs),
}

#[derive(Args, Clone)]
struct ConnArgs {
    #[arg(long, default_value = "localhost")]
    host: String,
    #[arg(long, default_value_t = 5432)]
    port: u16,
    #[arg(long, default_value = "sentient")]
    dbname: String,
    #[arg(long, default_value = "sentient")]
    user: String,
    #[arg(long, env = "PGPASSWORD", default_value = "")]
    password: String,
}

#[derive(Args)]
struct BackupArgs {
    #[command(flatten)]
    conn: ConnArgs,
    /// Output archive path.
    #[arg(short, long, default_value = "sentient.sentient-backup")]
    output: PathBuf,
    /// Category ids whose DATA to exclude (comma-separated). See `sbr categories`.
    /// Schema of every table is always kept; 'configuration' can't be skipped.
    #[arg(long, value_delimiter = ',')]
    skip: Vec<String>,
    /// Convenience alias for `--skip telemetry_historical`.
    #[arg(long)]
    no_telemetry: bool,
    /// Keep only the last N days of telemetry (default: all, unless skipped).
    #[arg(long)]
    telemetry_days: Option<u32>,
    /// Include the reports file store from this directory (if 'reports' isn't skipped).
    #[arg(long)]
    reports_path: Option<PathBuf>,
    /// Include the vc-repos file store from this directory (if 'version_control' isn't skipped).
    #[arg(long)]
    vc_repos_path: Option<PathBuf>,
    /// zstd compression level (1..=22).
    #[arg(long, default_value_t = 10)]
    level: i32,
    /// Password-lock (encrypt) the backup with age.
    #[arg(long = "encrypt-password", env = "SBR_BACKUP_PASSWORD")]
    encrypt_password: Option<String>,
}

#[derive(Args)]
struct RestoreArgs {
    #[command(flatten)]
    conn: ConnArgs,
    /// Backup archive to restore.
    #[arg(short, long)]
    input: PathBuf,
    /// Allow restoring into a non-empty database (unsafe; v1 default is empty-only).
    #[arg(long)]
    allow_nonempty: bool,
    /// Extract the reports file store (if present in the backup) to this directory.
    #[arg(long)]
    reports_path: Option<PathBuf>,
    /// Extract the vc-repos file store (if present) to this directory.
    #[arg(long)]
    vc_repos_path: Option<PathBuf>,
    /// Password for an encrypted backup.
    #[arg(long = "encrypt-password", env = "SBR_BACKUP_PASSWORD")]
    encrypt_password: Option<String>,
}

impl From<&ConnArgs> for ConnConfig {
    fn from(a: &ConnArgs) -> Self {
        ConnConfig {
            host: a.host.clone(),
            port: a.port,
            dbname: a.dbname.clone(),
            user: a.user.clone(),
            password: a.password.clone(),
        }
    }
}

/// CLI progress sink: steps → stdout, tool logs → stderr.
fn cli_sink() -> ProgressFn {
    Arc::new(|p: Progress| match p {
        Progress::Step { name, index, total } => println!("[{index}/{total}] {name}…"),
        Progress::Done { message } => println!("✓ {message}"),
        Progress::Log { line } => eprintln!("    {line}"),
        Progress::Percent { .. } => {}
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().cmd {
        Cmd::Categories => print_categories(),
        Cmd::Inspect(a) => inspect(&a).await?,
        Cmd::Backup(a) => {
            let mut skip = a.skip.clone();
            if a.no_telemetry {
                skip.push("telemetry_historical".into());
            }
            let mut selection = Selection::skipping(&skip);
            selection.telemetry_days = a.telemetry_days;
            let mut file_stores = Vec::new();
            if let Some(p) = &a.reports_path {
                if selection.is_included("reports") {
                    file_stores.push(FileStoreSpec {
                        id: "reports".into(),
                        category_id: "reports".into(),
                        path: p.clone(),
                    });
                }
            }
            if let Some(p) = &a.vc_repos_path {
                if selection.is_included("version_control") {
                    file_stores.push(FileStoreSpec {
                        id: "vc-repos".into(),
                        category_id: "version_control".into(),
                        path: p.clone(),
                    });
                }
            }
            let opts = BackupOptions {
                output: a.output.clone(),
                selection,
                file_stores,
                zstd_level: a.level,
                passphrase: a.encrypt_password.clone(),
            };
            let s = backup::run(&ConnConfig::from(&a.conn), &opts, cli_sink()).await?;
            println!(
                "  archive: {} ({})",
                s.output.display(),
                human_bytes(s.archive_bytes as i64)
            );
            println!("  dump sha256: {}", s.dump_sha256);
        }
        Cmd::Restore(a) => {
            let mut file_store_paths = Vec::new();
            if let Some(p) = &a.reports_path {
                file_store_paths.push(("reports".to_string(), p.clone()));
            }
            if let Some(p) = &a.vc_repos_path {
                file_store_paths.push(("vc-repos".to_string(), p.clone()));
            }
            let opts = RestoreOptions {
                input: a.input.clone(),
                allow_nonempty: a.allow_nonempty,
                file_store_paths,
                passphrase: a.encrypt_password.clone(),
            };
            let s = restore::run(&ConnConfig::from(&a.conn), &opts, cli_sink()).await?;
            println!(
                "  restored '{}' from {} ({} file store(s))",
                s.database,
                s.restored_from.display(),
                s.file_stores_restored
            );
        }
    }
    Ok(())
}

fn print_categories() {
    println!("Backup components ({} categories):\n", catalog().len());
    for c in catalog() {
        let flags = format!(
            "{}{}",
            if c.default_selected { "[x]" } else { "[ ]" },
            if c.locked { " (locked)" } else { "" }
        );
        println!("  {flags}  {:<34} {}", c.name, c.id);
        println!("        {}", c.notes);
        if let Some(fs) = c.file_store {
            println!("        + file store: {} ({})", fs.id, fs.default_path);
        }
    }
}

async fn inspect(a: &ConnArgs) -> Result<()> {
    let cfg = ConnConfig::from(a);
    let db = DbInspector::connect(&cfg).await?;
    let info = db.server_info().await?;
    println!("Connected to '{}'", info.database);
    println!("  {}", info.postgres_version.replace('\n', " "));
    println!(
        "  TimescaleDB: {}\n",
        info.timescaledb_version.as_deref().unwrap_or("(not installed)")
    );

    let tables = db.tables_with_true_sizes().await?;
    let report = build_report(&tables);
    println!(
        "  {:<38} {:>4} {:>7} {:>14} {:>10}",
        "COMPONENT", "SEL", "TABLES", "ROWS", "SIZE"
    );
    println!("  {}", "-".repeat(76));
    let (mut tb, mut tr) = (0i64, 0i64);
    for r in &report {
        tb += r.bytes;
        tr += r.rows;
        let sel = if r.locked {
            "req"
        } else if r.default_selected {
            "on"
        } else {
            "off"
        };
        println!(
            "  {:<38} {:>4} {:>7} {:>14} {:>10}",
            truncate(&r.name, 38),
            sel,
            r.tables.len(),
            r.rows,
            human_bytes(r.bytes)
        );
    }
    println!("  {}", "-".repeat(76));
    println!(
        "  {:<38} {:>4} {:>7} {:>14} {:>10}",
        "TOTAL", "", tables.len(), tr, human_bytes(tb)
    );

    println!("\n  File stores (backed up only when reachable from here):");
    for s in files::statuses() {
        println!(
            "    {:<10} {}  {}",
            s.id,
            if s.reachable { "[reachable]  " } else { "[unreachable]" },
            s.path
        );
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
