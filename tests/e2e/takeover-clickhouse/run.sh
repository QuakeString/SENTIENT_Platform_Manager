#!/usr/bin/env bash
# Takeover of SENTIENT's own docker/docker-compose.clickhouse.yml (the file a
# customer runs; only the SENTIENT image is the test stand-in), through the
# agent's real jobs, inside a Docker-enabled test server (e.g. the e2e
# container): add-existing, takeover-plan, take-over-compose; the PostgreSQL
# and ClickHouse data must survive and SENTIENT must answer.
#
#   tests/e2e/takeover-clickhouse/run.sh <container> <agent binary>
#
# The images spmtest/sentient:fake, postgres:18-alpine and
# clickhouse/clickhouse-server:24.8-alpine must be loaded in it.
set -euo pipefail
c=${1:?container}
agent=${2:?agent binary}
here=$(cd "$(dirname "$0")" && pwd)
docker cp "$agent" "$c:/usr/local/bin/spm-agent-under-test"
docker exec "$c" sh -c 'mkdir -p /home/st/SENTIENT/docker'
docker cp "$here/docker-compose.clickhouse.yml" "$c:/home/st/SENTIENT/docker/docker-compose.clickhouse.yml"
docker exec "$c" sh -euc '
cd /home/st/SENTIENT && docker compose -f docker/docker-compose.clickhouse.yml up -d >/dev/null 2>&1
for i in $(seq 60); do docker exec sentient-clickhouse wget -qO- http://127.0.0.1:8123/ping >/dev/null 2>&1 && break; sleep 2; done
docker exec sentient-postgres psql -U sentient -d sentient -qc "CREATE TABLE IF NOT EXISTS marker(v text); INSERT INTO marker VALUES ('"'"'before'"'"')"
docker exec sentient-clickhouse clickhouse-client --user sentient --password sentient -q "CREATE TABLE IF NOT EXISTS sentient.marker (v String) ENGINE=MergeTree ORDER BY v; INSERT INTO sentient.marker VALUES ('"'"'ch-before'"'"')"
spm-agent-under-test --job add-existing >/dev/null
plan=$(spm-agent-under-test --job takeover-plan | tail -1)
echo "$plan" | python3 -c "import json,sys; p=json.loads(sys.stdin.read()); assert not p[\"problems\"], p[\"problems\"]; print(\"plan ok: pg\", p[\"password_from\"], \"| ch\", p[\"ch_password_from\"])"
spm-agent-under-test --job take-over-compose | tail -1
for x in sentient sentient-postgres sentient-clickhouse; do
  test "$(docker inspect -f "{{index .Config.Labels \"com.docker.compose.project.config_files\"}}" $x)" = /opt/sentient/docker-compose.yml
done
test "$(docker exec sentient-postgres psql -U sentient -d sentient -tAc "SELECT v FROM marker")" = before
test "$(docker exec sentient-clickhouse sh -c "clickhouse-client --user sentient --password \"\$CLICKHOUSE_PASSWORD\" -q \"SELECT v FROM sentient.marker\"")" = ch-before
for i in $(seq 30); do curl -sf -o /dev/null http://127.0.0.1:8080/ && break; sleep 2; done
curl -sf -o /dev/null http://127.0.0.1:8080/
echo "PASS: taken over, data kept, SENTIENT answers"'
