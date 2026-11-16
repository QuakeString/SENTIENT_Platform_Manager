// Copyright © 2016-2026 The SENTIENT Authors

//! End-to-end restores across the three database types, against scratch
//! servers only (never a live SENTIENT). Ignored by default; run with
//!
//! ```text
//! SBR_E2E_DIR=<scratch dir with bin/pg_dump, bin/pg_restore> \
//! cargo test -p sentient-backup-core --test restore_e2e -- --ignored --nocapture
//! ```
//!
//! Servers (all user `sentient`, password `scratch`): PostgreSQL on 25432,
//! PostgreSQL + TimescaleDB on 25433, PostgreSQL for the ClickHouse
//! install on 25434 with ClickHouse's HTTP on 28123. Overridable with
//! SBR_E2E_PG / _TS / _HY / _CH. `SENTIENT_DIR` points at SENTIENT's
//! sources (data/sql, data/clickhouse).
//!
//! Every pair of source and target type, and per pair: everything,
//! configuration only (then Keep: carry_telemetry), telemetry only into a
//! running database, a date range, a damaged month left out; per target: a
//! forced failure midway and the free-space watch, both rolled back.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::TimeZone;
use sentient_backup_core::backup::{self, BackupOptions, Selection};
use sentient_backup_core::clickhouse::ChConn;
use sentient_backup_core::db::ConnConfig;
use sentient_backup_core::restore::{self, RestoreOptions, TelemetryInto};
use sentient_backup_core::restore_scope::{self as rs, Scope};
use sentient_backup_core::space::{DiskFree, SpaceWatch};
use sentient_backup_core::telemetry_move as mv;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T {
    Pg,
    Ts,
    Hy,
}

impl T {
    fn tag(self) -> &'static str {
        match self {
            T::Pg => "pg",
            T::Ts => "ts",
            T::Hy => "hy",
        }
    }
    fn port(self) -> u16 {
        let (var, def) = match self {
            T::Pg => ("SBR_E2E_PG", 25432),
            T::Ts => ("SBR_E2E_TS", 25433),
            T::Hy => ("SBR_E2E_HY", 25434),
        };
        std::env::var(var).ok().and_then(|p| p.parse().ok()).unwrap_or(def)
    }
}

const ALL: [T; 3] = [T::Pg, T::Ts, T::Hy];
const D1: &str = "11111111-1111-1111-1111-111111111111";
const D2: &str = "22222222-2222-2222-2222-222222222222";
const D3: &str = "33333333-3333-3333-3333-333333333333";

fn sentient_dir() -> PathBuf {
    PathBuf::from(std::env::var("SENTIENT_DIR").unwrap_or_else(|_| "/home/mz/INVENIA/SENTIENT".into()))
}
fn scratch() -> PathBuf {
    PathBuf::from(std::env::var("SBR_E2E_DIR").unwrap_or_else(|_| "/mnt/data_1/INVENIA/spm-wt-E/scratch".into()))
}
fn conn(t: T, db: &str) -> ConnConfig {
    ConnConfig { host: "127.0.0.1".into(), port: t.port(), dbname: db.into(), user: "sentient".into(), password: "scratch".into() }
}
fn ch(db: &str) -> ChConn {
    ChConn {
        url: format!("http://127.0.0.1:{}", std::env::var("SBR_E2E_CH").unwrap_or_else(|_| "28123".into())),
        database: db.into(),
        user: "sentient".into(),
        password: "scratch".into(),
    }
}
fn timescale_sql() -> String {
    std::fs::read_to_string(sentient_dir().join("data/sql/schema-ts-timescale.sql")).unwrap()
}
fn into(t: T) -> TelemetryInto {
    match t {
        T::Pg => TelemetryInto::Partitioned,
        T::Ts => TelemetryInto::Hypertable { timescale_sql: timescale_sql() },
        T::Hy => TelemetryInto::Clickhouse,
    }
}
fn ms(y: i32, m: u32, d: u32) -> i64 {
    chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp_millis()
}
const NOON: i64 = 12 * 3_600_000;
const DAY: i64 = 86_400_000;

async fn client(c: &ConnConfig) -> tokio_postgres::Client {
    mv::connect(c).await.unwrap()
}

async fn recreate_db(t: T, db: &str) {
    let admin = client(&conn(t, "postgres")).await;
    admin.batch_execute(&format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await.unwrap();
    admin.batch_execute(&format!("CREATE DATABASE {db}")).await.unwrap();
    if t == T::Hy {
        let c = ch("default");
        c.text(&format!("DROP DATABASE IF EXISTS {db} SYNC")).unwrap();
    }
}

fn ch_schema(db: &str) {
    let c = ch("default");
    c.text(&format!("DROP DATABASE IF EXISTS {db} SYNC")).unwrap();
    c.text(&format!("CREATE DATABASE {db}")).unwrap();
    let c = ch(db);
    for f in ["schema-ts.sql", "schema-ts-latest.sql", "schema-ts-latest-mv.sql", "schema-ts-latest-writes.sql", "schema-ts-latest-writes-mv.sql"] {
        let text = std::fs::read_to_string(sentient_dir().join("data/clickhouse").join(f)).unwrap();
        let body: String = text.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n");
        for stmt in body.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            c.text(stmt).unwrap();
        }
    }
}

/// Variant A (the backup): keys temp, hum, press; devices D1, D2; Aug..Oct.
/// Variant B (a running server): keys volt, temp, hum (other ids); D1, D3; Sep..mid Nov.
struct Seed {
    keys: [&'static str; 3],
    devices: [(&'static str, &'static str); 2],
    from: i64,
    days: i64,
    base: f64,
}
const A: Seed = Seed { keys: ["temp", "hum", "press"], devices: [(D1, "Press 6"), (D2, "Press 7")], from: 1_785_542_400_000, days: 92, base: 1000.0 };
const B: Seed = Seed { keys: ["volt", "temp", "hum"], devices: [(D1, "Press 6"), (D3, "Line 2 meter")], from: 1_788_220_800_000, days: 76, base: 2000.0 };

async fn seed(t: T, db: &str, s: &Seed) {
    assert_eq!(A.from, ms(2026, 8, 1));
    assert_eq!(B.from, ms(2026, 9, 1));
    recreate_db(t, db).await;
    let pg = client(&conn(t, db)).await;
    pg.batch_execute(
        "CREATE TABLE tb_schema_settings (schema_version bigint); INSERT INTO tb_schema_settings VALUES (30);
         CREATE TABLE key_dictionary (key varchar(255) NOT NULL, key_id serial UNIQUE, CONSTRAINT key_dictionary_id_pkey PRIMARY KEY (key));
         CREATE TABLE device (id uuid PRIMARY KEY, name varchar(255));
         CREATE TABLE asset (id uuid PRIMARY KEY, name varchar(255));
         CREATE TABLE attribute_kv (entity_id uuid, attribute_type int, attribute_key int, long_v bigint, last_update_ts bigint,
           CONSTRAINT attribute_kv_pkey PRIMARY KEY (entity_id, attribute_type, attribute_key));
         CREATE TABLE ts_kv_latest (entity_id uuid NOT NULL, key int NOT NULL, ts bigint NOT NULL, bool_v boolean, str_v varchar(10000000),
           long_v bigint, dbl_v double precision, json_v json, version bigint default 0, CONSTRAINT ts_kv_latest_pkey PRIMARY KEY (entity_id, key));",
    )
    .await
    .unwrap();
    match t {
        T::Pg => {
            mv::create_partitioned(&pg).await.unwrap();
            for w in mv::month_windows(ms(2026, 7, 1), ms(2026, 12, 1)) {
                let (y, m) = w.label.split_at(4);
                pg.batch_execute(&format!("CREATE TABLE ts_kv_{y}_{m} PARTITION OF ts_kv FOR VALUES FROM ({}) TO ({})", w.start, w.end)).await.unwrap();
            }
        }
        T::Ts => mv::create_hypertable(&pg, &timescale_sql()).await.unwrap(),
        T::Hy => {
            mv::create_plain(&pg).await.unwrap();
            ch_schema(db);
        }
    }
    for k in s.keys {
        pg.execute("INSERT INTO key_dictionary (key) VALUES ($1)", &[&k]).await.unwrap();
    }
    for (id, name) in s.devices {
        pg.execute("INSERT INTO device VALUES ($1::text::uuid, $2)", &[&id, &name]).await.unwrap();
    }
    // An attribute on the variant's third key, to see it keeps its name.
    pg.batch_execute(&format!(
        "INSERT INTO attribute_kv SELECT d.id, 1, k.key_id, 42, 0 FROM device d, key_dictionary k WHERE k.key = '{}'",
        s.keys[2]
    ))
    .await
    .unwrap();
    let ids: Vec<(String, i32)> = pg.query("SELECT key::text, key_id FROM key_dictionary", &[]).await.unwrap().iter().map(|r| (r.get(0), r.get(1))).collect();
    let mut tsv = String::new();
    for (dev, _) in s.devices {
        for k in s.keys {
            let kid = ids.iter().find(|(n, _)| n == k).unwrap().1;
            for d in 0..s.days {
                tsv.push_str(&format!("{dev}\t{kid}\t{}\t{}\n", s.from + d * DAY + NOON, s.base + d as f64));
            }
        }
    }
    if t == T::Hy {
        ch(db).insert("INSERT INTO ts_kv (entity_id, key, ts, dbl_v) FORMAT TabSeparated", tsv.as_bytes()).unwrap();
        // PostgreSQL's latest values too (the restore and the orphans read them).
        let copy = pg.copy_in::<_, bytes::Bytes>("COPY ts_kv_latest (entity_id, key, ts, dbl_v) FROM STDIN").await.unwrap();
        let latest: String = tsv
            .lines()
            .fold(std::collections::BTreeMap::<(String, String), String>::new(), |mut m, l| {
                let p: Vec<&str> = l.split('\t').collect();
                m.insert((p[0].into(), p[1].into()), l.to_string());
                m
            })
            .into_values()
            .map(|l| l + "\n")
            .collect();
        use futures_util::SinkExt;
        futures_util::pin_mut!(copy);
        copy.send(bytes::Bytes::from(latest)).await.unwrap();
        copy.finish().await.unwrap();
    } else {
        use futures_util::SinkExt;
        let copy = pg.copy_in::<_, bytes::Bytes>("COPY ts_kv (entity_id, key, ts, dbl_v) FROM STDIN").await.unwrap();
        futures_util::pin_mut!(copy);
        copy.send(bytes::Bytes::from(tsv)).await.unwrap();
        copy.finish().await.unwrap();
        pg.batch_execute(
            "INSERT INTO ts_kv_latest (entity_id, key, ts, bool_v, str_v, long_v, dbl_v, json_v) \
             SELECT DISTINCT ON (entity_id, key) entity_id, key, ts, bool_v, str_v, long_v, dbl_v, json_v FROM ts_kv ORDER BY entity_id, key, ts DESC",
        )
        .await
        .unwrap();
        if t == T::Ts {
            // Older chunks compressed, as SENTIENT's policy leaves them.
            pg.batch_execute(&format!(
                "SELECT compress_chunk(c, if_not_compressed => true) FROM show_chunks('ts_kv', older_than => {}::bigint) c",
                ms(2026, 10, 1)
            ))
            .await
            .unwrap();
        }
    }
}

/// History rows (ClickHouse: as the table keeps them, FINAL).
async fn rows(t: T, db: &str) -> i64 {
    if t == T::Hy {
        ch(db).text("SELECT count() FROM ts_kv FINAL").unwrap().trim().parse().unwrap()
    } else {
        client(&conn(t, db)).await.query_one("SELECT count(*) FROM ts_kv", &[]).await.unwrap().get(0)
    }
}

async fn raw_ch_rows(db: &str) -> i64 {
    ch(db).text("SELECT count() FROM ts_kv").unwrap().trim().parse().unwrap()
}

async fn key_id(t: T, db: &str, key: &str) -> Option<i32> {
    client(&conn(t, db)).await.query_opt("SELECT key_id FROM key_dictionary WHERE key = $1", &[&key]).await.unwrap().map(|r| r.get(0))
}

/// The value of one row, read through `dict_db`'s key names; `ch_db` is the
/// ClickHouse database holding the history (a ClickHouse target).
async fn value_via(t: T, dict_db: &str, ch_db: &str, dev: &str, key: &str, day: (i32, u32, u32)) -> Option<f64> {
    let ts = ms(day.0, day.1, day.2) + NOON;
    let kid = key_id(t, dict_db, key).await?;
    if t == T::Hy {
        let v = ch(ch_db).text(&format!("SELECT dbl_v FROM ts_kv FINAL WHERE entity_id = '{dev}' AND key = {kid} AND ts = {ts}")).unwrap();
        v.trim().parse().ok()
    } else {
        client(&conn(t, dict_db))
            .await
            .query_opt("SELECT dbl_v FROM ts_kv WHERE entity_id = $1::text::uuid AND key = $2 AND ts = $3", &[&dev, &kid, &ts])
            .await
            .unwrap()
            .map(|r| r.get(0))
    }
}
async fn value(t: T, db: &str, dev: &str, key: &str, day: (i32, u32, u32)) -> Option<f64> {
    value_via(t, db, db, dev, key, day).await
}

fn sink(lines: Arc<std::sync::Mutex<Vec<String>>>) -> sentient_backup_core::progress::ProgressFn {
    Arc::new(move |p| {
        if let sentient_backup_core::progress::Progress::Log { line } = p {
            lines.lock().unwrap().push(line);
        }
    })
}
fn quiet() -> sentient_backup_core::progress::ProgressFn {
    sentient_backup_core::progress::noop()
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = scratch().join("tmp").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn empty_dir(d: &Path) -> bool {
    std::fs::read_dir(d).map(|mut r| r.next().is_none()).unwrap_or(true)
}

async fn make_backup(s: T) -> PathBuf {
    let db = format!("src_{}", s.tag());
    seed(s, &db, &A).await;
    let dir = scratch().join("bk");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(format!("a_{}.sentient-backup", s.tag()));
    let opts = BackupOptions {
        output: out.clone(),
        selection: Selection::full(),
        clickhouse: (s == T::Hy).then(|| ch(&db)),
        ..Default::default()
    };
    backup::run(&conn(s, &db), &opts, quiet()).await.unwrap();
    out
}

fn opts(file: &Path, t: T, scope: Scope, ch_db: Option<&str>, temp: &Path) -> RestoreOptions {
    RestoreOptions {
        input: file.to_path_buf(),
        passphrase: None,
        clickhouse: ch_db.map(ch),
        telemetry_into: Some(into(t)),
        scope,
        temp_dir: Some(temp.to_path_buf()),
        ..Default::default()
    }
}

/// A restore beside: a new database (and a ClickHouse one cloned empty).
async fn beside(t: T, name: &str) {
    recreate_db(t, name).await;
    if t == T::Hy {
        sentient_backup_core::clickhouse::clone_schema(&ch(name), "tmpl").unwrap();
    }
}

struct Report(Vec<String>);
impl Report {
    fn row(&mut self, pair: &str, mode: &str, ok: bool, detail: String) {
        let line = format!("| {pair} | {mode} | {} | {detail} |", if ok { "pass" } else { "FAIL" });
        println!("{line}");
        self.0.push(line);
    }
}

macro_rules! check {
    ($rep:expr, $pair:expr, $mode:expr, $fails:expr, $cond:expr, $($detail:tt)*) => {{
        let ok = $cond;
        if !ok {
            $fails.push(format!("{} {}: {}", $pair, $mode, format!($($detail)*)));
        }
        $rep.row(&$pair, $mode, ok, format!($($detail)*));
    }};
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn restore_across_types() {
    let bin = scratch().join("bin");
    std::env::set_var("SBR_PG_DUMP", bin.join("pg_dump"));
    std::env::set_var("SBR_PG_RESTORE", bin.join("pg_restore"));
    std::env::set_var("PGPASSWORD", "scratch");
    ch_schema("tmpl");
    let mut rep = Report(Vec::new());
    let mut fails: Vec<String> = Vec::new();
    let mut backups = Vec::new();
    for s in ALL {
        backups.push((s, make_backup(s).await));
    }
    // A: 2 devices × 3 keys × 92 days; B: 2 × 3 × 76; both: D1 temp/hum Sep..Oct (61 days).
    let (a_rows, b_rows, both) = (552i64, 456i64, 122i64);

    for (s, file) in &backups {
        for t in ALL {
            let pair = format!("{} → {}", s.tag(), t.tag());
            let p = format!("{}_{}", s.tag(), t.tag());
            let tmp = tmp_dir(&p);
            let ch_of = |db: &str| (t == T::Hy).then(|| db.to_string());

            // Everything, beside.
            let db = format!("e_{p}");
            beside(t, &db).await;
            let r = restore::run(&conn(t, &db), &opts(file, t, Scope::Everything, ch_of(&db).as_deref(), &tmp), quiet()).await;
            let n = rows(t, &db).await;
            let v = value(t, &db, D2, "press", (2026, 10, 31)).await;
            check!(rep, pair, "everything", fails, r.is_ok() && n == a_rows && v == Some(1091.0) && empty_dir(&tmp), "{:?} rows {n} (want {a_rows}), D2 press 31 Oct {v:?}", r.as_ref().err());

            // Configuration only, then Keep (carry the running server's telemetry).
            let db = format!("c_{p}");
            beside(t, &db).await;
            let r = restore::run(&conn(t, &db), &opts(file, t, Scope::Configuration, ch_of(&db).as_deref(), &tmp), quiet()).await;
            let n = rows(t, &db).await;
            let latest: i64 = client(&conn(t, &db)).await.query_one("SELECT count(*) FROM ts_kv_latest", &[]).await.unwrap().get(0);
            let devices: i64 = client(&conn(t, &db)).await.query_one("SELECT count(*) FROM device", &[]).await.unwrap().get(0);
            check!(rep, pair, "configuration (clear)", fails, r.is_ok() && n == 0 && latest == 0 && devices == 2, "{:?} rows {n}, latest {latest}, devices {devices}", r.as_ref().err());
            let live = format!("l_{p}");
            seed(t, &live, &B).await;
            let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
            let r = rs::carry_telemetry(&conn(t, &live), &conn(t, &db), None, &sink(lines.clone())).await;
            let n = if t == T::Hy { rows(t, &live).await } else { rows(t, &db).await };
            // Through the restored dictionary: a ClickHouse server keeps its history in place.
            let volt = value_via(t, &db, &live, D3, "volt", (2026, 11, 15)).await;
            let temp_same = key_id(t, &db, "temp").await == key_id(t, &live, "temp").await;
            let attr: Option<String> = client(&conn(t, &db))
                .await
                .query_opt("SELECT k.key::text FROM attribute_kv a JOIN key_dictionary k ON k.key_id = a.attribute_key WHERE a.entity_id = $1::text::uuid", &[&D2])
                .await
                .unwrap()
                .map(|r| r.get(0));
            let restored_latest: i64 = client(&conn(t, &db)).await.query_one("SELECT count(*) FROM ts_kv_latest", &[]).await.unwrap().get(0);
            check!(
                rep, pair, "configuration + keep", fails,
                r.is_ok() && n == b_rows && volt == Some(2075.0) && temp_same && attr.as_deref() == Some("press") && restored_latest == 6,
                "{:?} history {n} (want {b_rows}), D3 volt 15 Nov {volt:?}, temp id same {temp_same}, D2 attribute key {attr:?}, latest {restored_latest}", r.as_ref().err()
            );
            let orphans = rs::orphans(&conn(t, &live), file, None, Some(&tmp), 10).await;
            check!(rep, pair, "orphans", fails, matches!(&orphans, Ok((n, 1)) if n == &["Line 2 meter".to_string()]) && empty_dir(&tmp), "{orphans:?}");

            // Telemetry only, into a running database.
            let live = format!("to_{p}");
            seed(t, &live, &B).await;
            let r = restore::run(&conn(t, &live), &opts(file, t, Scope::TelemetryOnly, ch_of(&live).as_deref(), &tmp), quiet()).await;
            let n = rows(t, &live).await;
            let want = a_rows + b_rows - both;
            let kept = value(t, &live, D1, "temp", (2026, 9, 15)).await;
            let added = value(t, &live, D2, "press", (2026, 8, 10)).await;
            let own = value(t, &live, D3, "volt", (2026, 11, 1)).await;
            let names: i64 = client(&conn(t, &live)).await.query_one("SELECT count(*) FROM key_dictionary", &[]).await.unwrap().get(0);
            check!(
                rep, pair, "telemetry only", fails,
                r.is_ok() && n == want && kept == Some(2014.0) && added == Some(1009.0) && own == Some(2061.0) && names == 4 && empty_dir(&tmp),
                "{:?} rows {n} (want {want}), overlap {kept:?} (live 2014 wins), added {added:?}, own {own:?}, key names {names}", r.as_ref().err()
            );

            // A date range: 10 Sep .. 20 Oct replaced.
            let live = format!("rg_{p}");
            seed(t, &live, &B).await;
            let scope = Scope::Range { from_ms: ms(2026, 9, 10), to_ms: ms(2026, 10, 20) };
            let r = restore::run(&conn(t, &live), &opts(file, t, scope, ch_of(&live).as_deref(), &tmp), quiet()).await;
            let n = rows(t, &live).await;
            let want = 240 + 216;
            let inr = value(t, &live, D1, "temp", (2026, 9, 15)).await;
            let gone = value(t, &live, D3, "volt", (2026, 9, 15)).await;
            let out = value(t, &live, D3, "volt", (2026, 11, 1)).await;
            let not_added = value(t, &live, D2, "press", (2026, 8, 10)).await;
            check!(
                rep, pair, "date range", fails,
                r.is_ok() && n == want && inr == Some(1045.0) && gone.is_none() && out == Some(2061.0) && not_added.is_none(),
                "{:?} rows {n} (want {want}), in range {inr:?}, live-only in range {gone:?}, outside {out:?}, backup outside {not_added:?}", r.as_ref().err()
            );

            // A damaged month left out (telemetry only, October skipped).
            let live = format!("sk_{p}");
            seed(t, &live, &B).await;
            let manifest = restore::inspect(file, None).unwrap();
            let oct: Vec<String> = manifest.files.iter().map(|f| f.path.clone()).filter(|p| rs::member_month(p) == Some((2026, 10))).collect();
            let mut o = opts(file, t, Scope::TelemetryOnly, ch_of(&live).as_deref(), &tmp);
            o.skip_members = oct.clone();
            let r = restore::run(&conn(t, &live), &o, quiet()).await;
            let n = rows(t, &live).await;
            let want = a_rows + b_rows - both - 124;
            check!(rep, pair, "month left out", fails, r.is_ok() && n == want && oct.len() == 1, "{:?} rows {n} (want {want}), skipped {oct:?}", r.as_ref().err());
        }
    }

    // Per target: a forced failure midway, and the space watch.
    for t in ALL {
        let file = &backups.iter().find(|(s, _)| *s == t).unwrap().1;
        let pair = format!("{} → {}", t.tag(), t.tag());
        let tmp = tmp_dir(&format!("fail_{}", t.tag()));
        let ch_of = |db: &str| (t == T::Hy).then(|| db.to_string());
        for (scope, mode) in [(Scope::TelemetryOnly, "failure: telemetry only"), (Scope::Range { from_ms: ms(2026, 9, 10), to_ms: ms(2026, 10, 20) }, "failure: date range")] {
            let live = format!("f_{}", t.tag());
            seed(t, &live, &B).await;
            let point = if t == T::Hy { "ch_after_insert" } else { "pg_before_commit" };
            std::env::set_var("SBR_FAIL_AT", point);
            let r = restore::run(&conn(t, &live), &opts(file, t, scope, ch_of(&live).as_deref(), &tmp), quiet()).await;
            std::env::remove_var("SBR_FAIL_AT");
            let n = rows(t, &live).await;
            let raw = if t == T::Hy { raw_ch_rows(&live).await } else { n };
            let names: i64 = client(&conn(t, &live)).await.query_one("SELECT count(*) FROM key_dictionary", &[]).await.unwrap().get(0);
            let v = value(t, &live, D1, "temp", (2026, 9, 15)).await;
            let staging_left = if t == T::Hy { ch(&live).text("EXISTS TABLE spm_restore_range").unwrap().trim() == "1" } else { false };
            check!(
                rep, pair, mode, fails,
                r.is_err() && n == b_rows && raw == b_rows && v == Some(2014.0) && !staging_left && empty_dir(&tmp),
                "{} at {point}; rows {n}, stored {raw} (want {b_rows}), D1 temp {v:?}, key names {names}", r.as_ref().err().map(|e| e.to_string()).unwrap_or_else(|| "no error".into())
            );
        }
        // The space watch: the disk "falls" on the third reading.
        let reads = Arc::new(AtomicUsize::new(0));
        let mk_watch = |reads: Arc<AtomicUsize>| {
            let mut w = SpaceWatch::new(move || {
                let n = reads.fetch_add(1, Ordering::SeqCst);
                vec![DiskFree { label: "/mnt/disk-a (PostgreSQL)".into(), free: if n >= 2 { 1_900_000_000 } else { 50_000_000_000 } }]
            });
            w.every = std::time::Duration::from_millis(30);
            w
        };
        let live = format!("w_{}", t.tag());
        seed(t, &live, &B).await;
        let mut o = opts(file, t, Scope::TelemetryOnly, ch_of(&live).as_deref(), &tmp);
        o.watch = Some(mk_watch(reads.clone()));
        let r = restore::run(&conn(t, &live), &o, quiet()).await;
        let n = rows(t, &live).await;
        let raw = if t == T::Hy { raw_ch_rows(&live).await } else { n };
        let space = matches!(&r, Err(sentient_backup_core::Error::SpaceLow(m)) if m.contains("/mnt/disk-a (PostgreSQL) fell to 1.9 GB"));
        check!(rep, pair, "space watch: telemetry only", fails, space && n == b_rows && raw == b_rows && empty_dir(&tmp), "{r:?}; rows {n}, stored {raw}");
        let db = format!("we_{}", t.tag());
        beside(t, &db).await;
        let mut o = opts(file, t, Scope::Everything, ch_of(&db).as_deref(), &tmp);
        o.watch = Some(mk_watch(Arc::new(AtomicUsize::new(0))));
        let r = restore::run(&conn(t, &db), &o, quiet()).await;
        let space = matches!(&r, Err(sentient_backup_core::Error::SpaceLow(_)));
        check!(rep, pair, "space watch: everything", fails, space && empty_dir(&tmp), "{r:?}");
    }

    let table = format!("| pair | mode | result | detail |\n|---|---|---|---|\n{}\n", rep.0.join("\n"));
    if let Ok(out) = std::env::var("SBR_E2E_REPORT") {
        std::fs::write(out, &table).unwrap();
    }
    assert!(fails.is_empty(), "{} failed:\n{}", fails.len(), fails.join("\n"));
}
