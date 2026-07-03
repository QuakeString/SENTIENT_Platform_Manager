//! Database connection + inspection: enumerate tables, sizes and row counts,
//! and reconcile them against the category model so the UI (and CLI) can show
//! "how big is each backup component".

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use serde::Serialize;
use tokio_postgres::{Client, NoTls};

use crate::categories::{self, catalog, CategoryKind};
use crate::error::{Error, Result};
use crate::util::HashingWriter;

/// Connection parameters. (TLS is a later phase; local/dev is NoTls for now.)
#[derive(Debug, Clone, Serialize)]
pub struct ConnConfig {
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub user: String,
    #[serde(skip)]
    pub password: String,
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 5432,
            dbname: "sentient".into(),
            user: "sentient".into(),
            password: String::new(),
        }
    }
}

/// Server identity — used for the backup manifest and restore compatibility.
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub database: String,
    pub postgres_version: String,
    pub timescaledb_version: Option<String>,
}

/// One live table's footprint.
#[derive(Debug, Clone, Serialize)]
pub struct TableInfo {
    pub name: String,
    pub bytes: i64,
    pub rows: i64,
}

/// A category rolled up over the live tables that belong to it.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryReport {
    pub id: String,
    pub name: String,
    pub kind: CategoryKind,
    pub default_selected: bool,
    pub locked: bool,
    pub tables: Vec<String>,
    pub bytes: i64,
    pub rows: i64,
    pub notes: String,
    pub file_store_id: Option<String>,
}

/// Connects and answers inspection queries.
pub struct DbInspector {
    client: Client,
}

impl DbInspector {
    /// Open a connection. The background driver task is spawned and detached;
    /// it lives as long as `client` (dropped with the inspector).
    pub async fn connect(cfg: &ConnConfig) -> Result<Self> {
        let conn_str = format!(
            "host={} port={} dbname={} user={} password={} application_name=sentient-backup",
            cfg.host, cfg.port, cfg.dbname, cfg.user, cfg.password
        );
        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .map_err(|e| Error::Connect(e.to_string()))?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::warn!("postgres connection error: {e}");
            }
        });
        Ok(Self { client })
    }

    /// Run one or more statements, ignoring results (for pre/post-restore SQL).
    pub async fn batch(&self, sql: &str) -> Result<()> {
        self.client.batch_execute(sql).await?;
        Ok(())
    }

    /// Stream a `COPY (...) TO STDOUT (FORMAT binary)` into a zstd file, hashing
    /// the compressed output. Returns (sha256, compressed_bytes, raw_bytes).
    pub async fn copy_out_compressed(
        &self,
        sql: &str,
        dest: &Path,
        level: i32,
    ) -> Result<(String, u64, u64)> {
        use futures_util::StreamExt;
        let stream = self.client.copy_out(sql).await?;
        futures_util::pin_mut!(stream);
        let file = File::create(dest)?;
        let mut hw = HashingWriter::new(file);
        let mut enc =
            zstd::stream::Encoder::new(&mut hw, level).map_err(|e| Error::msg(format!("zstd: {e}")))?;
        let mut raw = 0u64;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            raw += bytes.len() as u64;
            enc.write_all(&bytes)?;
        }
        enc.finish().map_err(|e| Error::msg(format!("zstd finish: {e}")))?;
        let (sha, comp) = hw.finish();
        Ok((sha, comp, raw))
    }

    /// Feed a zstd-compressed COPY payload into `COPY <table> FROM STDIN (FORMAT
    /// binary)`. Returns rows inserted.
    pub async fn copy_in_compressed(&self, sql: &str, src: &Path) -> Result<u64> {
        use futures_util::SinkExt;
        let sink = self.client.copy_in::<_, bytes::Bytes>(sql).await?;
        futures_util::pin_mut!(sink);
        let f = File::open(src)?;
        let mut dec =
            zstd::stream::Decoder::new(f).map_err(|e| Error::msg(format!("zstd: {e}")))?;
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = dec.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sink.send(bytes::Bytes::copy_from_slice(&buf[..n])).await?;
        }
        let rows = sink.finish().await?;
        Ok(rows)
    }

    pub async fn server_info(&self) -> Result<ServerInfo> {
        let row = self
            .client
            .query_one("SELECT current_database(), version()", &[])
            .await?;
        let database: String = row.get(0);
        let postgres_version: String = row.get(1);

        let ts: Option<String> = self
            .client
            .query_opt(
                "SELECT extversion FROM pg_extension WHERE extname = 'timescaledb'",
                &[],
            )
            .await?
            .map(|r| r.get(0));

        Ok(ServerInfo {
            database,
            postgres_version,
            timescaledb_version: ts,
        })
    }

    /// All ordinary/partitioned tables in `public`, with total relation size and
    /// live row estimate. Hypertable parents report their parent-only size here;
    /// `hypertable_stats()` provides the real (chunk-inclusive) figures.
    pub async fn list_public_tables(&self) -> Result<Vec<TableInfo>> {
        let rows = self
            .client
            .query(
                "SELECT c.relname,
                        pg_total_relation_size(c.oid)::int8,
                        COALESCE(s.n_live_tup, 0)::int8
                 FROM pg_class c
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 LEFT JOIN pg_stat_user_tables s ON s.relid = c.oid
                 WHERE n.nspname = 'public'
                   AND c.relkind IN ('r', 'p')
                 ORDER BY c.relname",
                &[],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| TableInfo {
                name: r.get(0),
                bytes: r.get(1),
                rows: r.get(2),
            })
            .collect())
    }

    /// Real hypertable sizes/rows (chunk-inclusive). Empty if TimescaleDB is
    /// absent. Keyed by hypertable name.
    pub async fn hypertable_stats(&self) -> BTreeMap<String, (i64, i64)> {
        let q = "SELECT h.hypertable_name,
                        hypertable_size(c.oid::regclass)::int8,
                        approximate_row_count(c.oid::regclass)::int8
                 FROM timescaledb_information.hypertables h
                 JOIN pg_namespace n ON n.nspname = h.hypertable_schema
                 JOIN pg_class c ON c.relname = h.hypertable_name
                                AND c.relnamespace = n.oid";
        match self.client.query(q, &[]).await {
            Ok(rows) => rows
                .into_iter()
                .map(|r| {
                    let name: String = r.get(0);
                    (name, (r.get::<_, i64>(1), r.get::<_, i64>(2)))
                })
                .collect(),
            Err(e) => {
                tracing::debug!("hypertable_stats unavailable (no timescaledb?): {e}");
                BTreeMap::new()
            }
        }
    }

    /// Convenience: list tables with hypertable figures merged in.
    pub async fn tables_with_true_sizes(&self) -> Result<Vec<TableInfo>> {
        let mut tables = self.list_public_tables().await?;
        let ht = self.hypertable_stats().await;
        for t in &mut tables {
            if let Some((bytes, rows)) = ht.get(&t.name) {
                t.bytes = *bytes;
                t.rows = *rows;
            }
        }
        Ok(tables)
    }
}

/// Create a new (empty) database, for the restore flow's "make a fresh target"
/// step. Connects to the `postgres` maintenance database with the same
/// credentials (CREATE DATABASE can't run from within the target itself), so
/// the user doesn't need psql. Fails if the name already exists.
pub async fn create_database(cfg: &ConnConfig, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::msg("database name is empty"));
    }
    let mut maint = cfg.clone();
    maint.dbname = "postgres".into();
    let db = DbInspector::connect(&maint).await?;
    // DDL can't be parameterized — quote as an identifier (doubling any ").
    let quoted = format!("\"{}\"", name.replace('"', "\"\""));
    db.batch(&format!("CREATE DATABASE {quoted}")).await?;
    Ok(())
}

/// Roll live tables up into category reports (in catalog order).
pub fn build_report(tables: &[TableInfo]) -> Vec<CategoryReport> {
    // accumulate per category id
    let mut acc: BTreeMap<&'static str, (Vec<String>, i64, i64)> = BTreeMap::new();
    for t in tables {
        let cat = categories::category_for_table(&t.name);
        let e = acc.entry(cat).or_default();
        e.0.push(t.name.clone());
        e.1 += t.bytes;
        e.2 += t.rows;
    }
    catalog()
        .iter()
        .map(|c| {
            let (mut names, bytes, rows) = acc.remove(c.id).unwrap_or_default();
            names.sort();
            CategoryReport {
                id: c.id.into(),
                name: c.name.into(),
                kind: c.kind,
                default_selected: c.default_selected,
                locked: c.locked,
                tables: names,
                bytes,
                rows,
                notes: c.notes.into(),
                file_store_id: c.file_store.map(|f| f.id.into()),
            }
        })
        .collect()
}

/// Human-readable byte size.
pub fn human_bytes(b: i64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
