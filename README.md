# SENTIENT Platform Manager

One desktop app to **install, run, update, and back up** the SENTIENT platform on
a machine. It replaces the former standalone *SENTIENT Installer* and *SENTIENT
Backup* apps, reusing their engines.

- **Setup** — install wizard: system check → choose components (Platform, and
  optionally the Client) → WSL2 → Docker Engine → deploy the stack.
- **Status / Update** — start/stop/logs and update the running stack.
- **Backup / Restore** — selective, encrypted DB backup & restore (local or
  remote SENTIENT).

Windows-first (Docker Engine inside WSL2, no Docker Desktop). Built with Rust +
Tauri. See [docs/RESEARCH_AND_PLAN.md](docs/RESEARCH_AND_PLAN.md).
