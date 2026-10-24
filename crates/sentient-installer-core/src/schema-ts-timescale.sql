--
-- Copyright © 2016-2025 The SENTIENT Authors
--
-- Licensed under the Apache License, Version 2.0 (the "License");
-- you may not use this file except in compliance with the License.
-- You may obtain a copy of the License at
--
--     http://www.apache.org/licenses/LICENSE-2.0
--
-- Unless required by applicable law or agreed to in writing, software
-- distributed under the License is distributed on an "AS IS" BASIS,
-- WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
-- See the License for the specific language governing permissions and
-- limitations under the License.
--

-- TimescaleDB conversion for ts_kv - applied ONLY in PostgreSQL history mode
-- (TS_TYPE=sql / timescale), never in hybrid.
--
-- Split out of schema-ts-psql.sql because that file ran unconditionally and its
-- CREATE EXTENSION made every install require the timescale image. ThingsBoard
-- keeps the same separation (SqlTsDatabaseSchemaService -> schema-ts-psql.sql,
-- TimescaleTsDatabaseSchemaService -> schema-timescale.sql); this restores it.
--
-- In hybrid mode history lives in ClickHouse and ts_kv stays empty, so there is
-- nothing for chunking, compression or drop_chunks to act on. On the
-- single-board tier ts_kv IS the history table and these policies are
-- load-bearing: drop_chunks removes millions of rows in milliseconds where the
-- equivalent DELETE takes hundreds.
--
-- Runs after schema-ts-psql.sql has created ts_kv as a plain table;
-- create_hypertable converts it in place while it is still empty.

CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;

-- Convert ts_kv to TimescaleDB hypertable.
-- Default chunk_time_interval = 7 days (604800000 ms). Override at install time
-- via the SENTIENT_TS_KV_CHUNK_DAYS environment variable; the schema manager
-- calls set_chunk_time_interval() if a non-default value is set.
-- `create_default_indexes => false` suppresses the `ts_kv_ts_idx (ts DESC)`
-- index TimescaleDB would otherwise add on its own. That default suits a
-- metrics workload that scans all series over a time window; every read here is
-- scoped to an entity and served by `ts_kv_pkey (entity_id, key, ts)`, so the
-- extra B-tree earned nothing while costing a write per row and ~900 MB.
--
-- It was also actively harmful: being narrower than the primary key, the
-- planner preferred it for the TTL delete's `ts < cutoff` predicate, which
-- discards the entity_id pruning the primary key provides. Time-range pruning
-- does not depend on it — Timescale excludes whole chunks from the dimension
-- metadata, with no index involved.
SELECT create_hypertable('ts_kv', by_range('ts', 604800000::bigint),
                         create_default_indexes => false,
                         if_not_exists => true);

-- Tell TimescaleDB what "now" means for this hypertable.
--
-- `ts` is a bigint of epoch milliseconds, not a timestamptz, so the hypertable is
-- partitioned on an INTEGER range. Any policy that expresses its threshold as a
-- lag from the present -- compression (`compress_after`) and retention
-- (`drop_after`) -- has no way to evaluate that lag on an integer dimension
-- unless an "integer now" function is registered. Without it every policy run
-- aborts with `integer_now function not set` and silently never does any work.
CREATE OR REPLACE FUNCTION ts_kv_now_ms() RETURNS bigint
    LANGUAGE sql STABLE AS
$$
SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint;
$$;

SELECT set_integer_now_func('ts_kv', 'ts_kv_now_ms', replace_if_exists => true);

-- Enable compression for older data.
-- compress_segmentby groups (entity_id, key) into one columnar segment per
-- device+metric series; compress_orderby ts DESC enables delta encoding for
-- 5-10x better compression ratios.
ALTER TABLE ts_kv SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'entity_id, key',
    timescaledb.compress_orderby = 'ts DESC'
);

-- Add compression policy. Default: compress chunks older than 4 weeks
-- (28 days = 2419200000 ms). The recent 4 weeks remain uncompressed for fast
-- random reads/writes/modifies. Override at install time via the
-- SENTIENT_TS_KV_COMPRESS_AFTER_DAYS environment variable.
SELECT add_compression_policy('ts_kv', 2419200000::bigint, if_not_exists => true);

-- Retention policy can be added manually if needed:
-- SELECT add_retention_policy('ts_kv', 63072000000::bigint, if_not_exists => true);
-- For now, we skip automatic retention - it can be configured per-tenant
