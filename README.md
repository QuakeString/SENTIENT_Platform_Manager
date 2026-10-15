# SENTIENT Platform Manager

One desktop app to **install, run, update, and back up** the SENTIENT platform on
a machine. It replaces the former standalone *SENTIENT Installer* and *SENTIENT
Backup* apps, reusing their engines.

- **Setup** — install wizard: system check → how to run it and which
  time-series storage → configuration (recommended, or every knob the reference
  compose files expose) → automated install.
- **Status / Update** — start/stop/logs and update the running stack.
- **Backup / Restore** — selective, encrypted DB backup & restore (local or
  remote SENTIENT).

The wizard adapts to the host:

| Host    | Modes                                                    | Storage flavours                                  |
|---------|----------------------------------------------------------|---------------------------------------------------|
| Windows | **Directly on Windows** (services, offline bundle) · Docker in WSL2 | native: PostgreSQL + TimescaleDB · Docker: any |
| Linux   | **Docker on this machine** (recommended) · natively with systemd | Docker: any · native: PostgreSQL + ClickHouse, PostgreSQL only |

PostgreSQL + ClickHouse is the default everywhere the engine can honour it —
the reference deployment. The Docker engine writes the same stack as
SENTIENT's `docker/docker-compose.<flavor>.yml`; the Linux native engine takes
PostgreSQL from the distribution, ClickHouse from the vendor's repository or
static build, and runs SENTIENT as a systemd unit under `/opt/sentient`.

Built with Rust + Tauri. See [docs/RESEARCH_AND_PLAN.md](docs/RESEARCH_AND_PLAN.md).

## Headless harnesses

The engines are UI-agnostic; `crates/sentient-installer-core` ships small
drivers for exercising them without the app:

- `sicheck` — the preflight checks.
- `sidocker <cmd> <config.json>` — the Docker engine (`compose`, `deploy`,
  `status`, `logs`, `control`, `update`, `cleanup`, `uninstall`); the JSON is a
  partial `DeployConfig`, missing keys take the reference defaults.
- `sinative <cmd> <root|->` — the native engine (Windows services or Linux
  systemd). `SINATIVE_FLAVOR`, `SINATIVE_LICENSE_SERVER_URL` and
  `SINATIVE_EXTRA_ENV` steer a test run.
- `packaging/linux/build-payload.sh <sentient-repo> <out>` builds the
  `sentient-payload-linux-<arch>.tar` the Linux native engine installs from.
