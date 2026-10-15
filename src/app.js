// Frontend for the SENTIENT Backup & Restore desktop app. Uses Tauri's global
// `invoke` / `Channel` (withGlobalTauri = true) — no bundler.

const invoke = window.__TAURI__?.core?.invoke;
const Channel = window.__TAURI__?.core?.Channel;

const $ = (id) => document.getElementById(id);

// Append a line to a <pre> log, capping it to the last N lines so a flood of
// output (e.g. `docker compose pull` progress) can't grow the DOM unbounded and
// spike CPU. Trims only when it gets big, to keep the common path cheap.
const LOG_MAX_LINES = 400;
function logAppend(el, line) {
  el.textContent += line + "\n";
  if (el.textContent.length > 48000) {
    const lines = el.textContent.split("\n");
    if (lines.length > LOG_MAX_LINES) {
      el.textContent = lines.slice(lines.length - LOG_MAX_LINES).join("\n");
    }
  }
  el.scrollTop = el.scrollHeight;
}

let categories = []; // last inspect result
let restoreFile = null; // chosen archive path
let profilesList = []; // saved connection profiles

// ---- Theme: follow OS by default, manual override persisted in localStorage --
function effectiveDark() {
  const t = localStorage.getItem("theme");
  if (t === "dark") return true;
  if (t === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}
function refreshThemeIcon() {
  const use = $("themeIcon");
  if (use) use.setAttribute("href", effectiveDark() ? "#i-sun" : "#i-moon");
}
function currentThemeMode() { return localStorage.getItem("theme") || "auto"; }
function syncThemeRadios() {
  const m = currentThemeMode();
  document.querySelectorAll('input[name="themeMode"]').forEach((r) => (r.checked = r.value === m));
}
function applyThemeMode(mode) {
  if (mode === "auto") {
    localStorage.removeItem("theme");
    document.documentElement.removeAttribute("data-theme");
  } else {
    localStorage.setItem("theme", mode);
    document.documentElement.setAttribute("data-theme", mode);
  }
  refreshThemeIcon();
  syncThemeRadios();
}
function initTheme() {
  const t = localStorage.getItem("theme");
  if (t) document.documentElement.setAttribute("data-theme", t);
  refreshThemeIcon();
  syncThemeRadios();
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (!localStorage.getItem("theme")) refreshThemeIcon();
  });
}
function toggleTheme() { applyThemeMode(effectiveDark() ? "light" : "dark"); }

function humanBytes(b) {
  const u = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${u[i]}`;
}

function conn() {
  return {
    host: $("host").value,
    port: Number($("port").value),
    dbname: $("dbname").value,
    user: $("user").value,
    password: $("password").value,
  };
}

// ---- Reusable progress widget (animated bar + collapsible verbose log) -------
function ProgressView(prefix) {
  const el = (s) => $(prefix + s);
  const bar = () => el("Bar");
  const fill = () => bar().querySelector(".fill");

  el("Toggle").addEventListener("click", () => {
    const log = el("Log");
    const open = log.style.display !== "none";
    log.style.display = open ? "none" : "";
    el("Toggle").textContent = (open ? "▸" : "▾") + " Details";
  });

  return {
    start() {
      el("Progress").style.display = "";
      el("Log").textContent = "";
      el("Step").textContent = "Starting…";
      bar().classList.remove("err");
      bar().classList.add("active");
      fill().style.width = "4%";
    },
    message(p) {
      if (p.type === "step") {
        el("Step").textContent = `[${p.index}/${p.total}] ${p.name}`;
        fill().style.width = Math.round((p.index / p.total) * 100) + "%";
      } else if (p.type === "log") {
        logAppend(el("Log"), p.line);
      } else if (p.type === "done") {
        el("Step").textContent = "✓ " + p.message;
        fill().style.width = "100%";
        bar().classList.remove("active");
      }
    },
    succeed() {
      bar().classList.remove("active");
      fill().style.width = "100%";
    },
    fail() {
      bar().classList.remove("active");
      bar().classList.add("err");
    },
    channel() {
      const ch = new Channel();
      ch.onmessage = (p) => this.message(p);
      return ch;
    },
  };
}
const backupProgress = ProgressView("b");
const restoreProgress = ProgressView("r");

// ---- Connection / inspect ----------------------------------------------------
function recalcTotal() {
  let bytes = 0, rows = 0, tables = 0;
  document.querySelectorAll("input.cat[type=checkbox]").forEach((cb) => {
    if (cb.checked) {
      const c = categories[Number(cb.dataset.i)];
      bytes += c.bytes; rows += c.rows; tables += c.tables.length;
    }
  });
  $("tSize").textContent = humanBytes(bytes);
  $("tRows").textContent = rows.toLocaleString();
  $("tTables").textContent = tables;
}

function syncTeleOpts() {
  const cb = document.querySelector('input.cat[data-cid="telemetry_historical"]');
  $("teleOpts").style.display = cb && cb.checked ? "" : "none";
}

function renderCategories() {
  const tbody = $("cats");
  tbody.innerHTML = "";
  categories.forEach((c, i) => {
    const tr = document.createElement("tr");
    const checked = c.default_selected ? "checked" : "";
    const disabled = c.locked ? "disabled" : "";
    tr.innerHTML = `
      <td><input class="cat" type="checkbox" data-i="${i}" data-cid="${c.id}" ${checked} ${disabled} /></td>
      <td>${c.name}${c.locked ? " <span class='badge'>required</span>" : ""}
          <div class="cat-note">${c.notes}</div></td>
      <td>${c.tables.length}</td>
      <td class="rows">${c.rows.toLocaleString()}</td>
      <td class="size">${humanBytes(c.bytes)}</td>`;
    tbody.appendChild(tr);
  });
  tbody.querySelectorAll("input.cat").forEach((cb) =>
    cb.addEventListener("change", () => { recalcTotal(); syncTeleOpts(); })
  );
  recalcTotal();
  syncTeleOpts();
}

async function connect(navigate = true, quiet = false) {
  if (!invoke) { setStatus("Not running inside Tauri.", true); return; }
  $("connect").disabled = true;
  if (!quiet) setStatus("Connecting…");
  try {
    const res = await invoke("inspect", conn());
    categories = res.categories;
    connected = true;
    setConnStatus(`Connected — ${res.server.database} (${res.table_count} tables, ${humanBytes(res.total_bytes)})`, "ok");
    setStatus(
      `Connected to '${res.server.database}' — ${res.server.postgres_version.split(" on ")[0]}` +
      (res.server.timescaledb_version ? `, TimescaleDB ${res.server.timescaledb_version}` : "") +
      ` — ${res.table_count} tables, ${humanBytes(res.total_bytes)} total.`
    );
    const c = conn();
    // Remember the last connection so it auto-fills + reconnects next launch.
    try {
      await invoke("setting_set", {
        key: "last_conn",
        value: JSON.stringify({ host: c.host, port: c.port, dbname: c.dbname, user: c.user }),
      });
      await invoke("set_last_password", { password: c.password || "" });
    } catch { /* store unavailable */ }
    renderCategories();
    updateGating();
    if (navigate) showView("backup");
  } catch (e) {
    connected = false;
    if (!quiet) {
      setStatus("Error: " + e, true);
      setConnStatus("Connection failed", "err");
    }
    updateGating();
  } finally {
    $("connect").disabled = false;
  }
}

function setStatus(msg, isErr) {
  const s = $("status");
  s.textContent = msg;
  s.classList.toggle("err", !!isErr);
}

// ---- Navigation --------------------------------------------------------------
let connected = false;

function showView(name) {
  document.querySelectorAll(".sidebar .nav").forEach((b) =>
    b.classList.toggle("active", b.dataset.view === name)
  );
  document.querySelectorAll(".page").forEach((p) => p.classList.remove("active"));
  const page = $(name + "Page");
  if (page) page.classList.add("active");
  if (name === "history") loadHistory();
  if (name === "status") loadStatus();
  if (name === "update") refreshUpdateGate();
}

function setConnStatus(text, state) {
  const el = $("connStatus");
  el.className = "conn-status" + (state ? " " + state : "");
  el.innerHTML = `<span class="dot"></span> ${text}`;
}

function updateGating() {
  $("backupNeedsConn").style.display = connected ? "none" : "";
  $("backupBody").style.display = connected ? "" : "none";
  $("restoreNeedsConn").style.display = connected ? "none" : "";
  $("restoreBody").style.display = connected ? "" : "none";
}

// ---- Saved connection profiles ----------------------------------------------
async function loadProfiles() {
  try { profilesList = await invoke("list_connections"); } catch { profilesList = []; }
  const sel = $("profiles");
  const cur = sel.value;
  sel.innerHTML = '<option value="">— saved connections —</option>' +
    profilesList.map((p) => `<option value="${p.id}">${p.name}</option>`).join("");
  sel.value = cur;
}

async function onProfileSelect() {
  const id = Number($("profiles").value);
  if (!id) return;
  const p = profilesList.find((x) => x.id === id);
  if (!p) return;
  $("host").value = p.host; $("port").value = p.port;
  $("dbname").value = p.dbname; $("user").value = p.username;
  $("password").value = "";
  if (p.has_password) {
    try {
      const pw = await invoke("get_connection_password", { id });
      if (pw != null) $("password").value = pw;
    } catch { /* keychain unavailable */ }
  }
}

async function saveProfile() {
  const c = conn();
  const name = `${c.user}@${c.host}:${c.port}/${c.dbname}`;
  const existing = profilesList.find((p) => p.name === name);
  try {
    const res = await invoke("save_connection", {
      profile: {
        id: existing ? existing.id : null,
        name, host: c.host, port: c.port, dbname: c.dbname,
        username: c.user, password: c.password || null,
      },
    });
    await loadProfiles();
    $("profiles").value = String(res.id);
    if (c.password && !res.password_saved) {
      setStatus(`Saved '${name}' — but the password could not be stored: no system keychain is available. ` +
        `On Linux, run a Secret Service (install gnome-keyring, or enable KWallet's Secret Service).`, true);
    } else {
      setStatus(`Saved '${name}'` + (res.password_saved ? " (password in the OS keychain)." : "."));
    }
  } catch (e) { setStatus("Save failed: " + e, true); }
}

async function deleteProfile() {
  const id = Number($("profiles").value);
  if (!id) return;
  try { await invoke("delete_connection", { id }); await loadProfiles(); $("profiles").value = ""; }
  catch (e) { setStatus("Delete failed: " + e, true); }
}

// ---- History -----------------------------------------------------------------
function fmtWhen(ts) { try { return new Date(ts).toLocaleString(); } catch { return ts; } }
function statusBadge(s) {
  const color = s === "success" ? "var(--accent)" : "var(--err)";
  return `<span style="color:${color}; font-weight:600">${s || ""}</span>`;
}
function baseName(p) { return (p || "").split(/[\\/]/).pop(); }

async function loadHistory() {
  try {
    const b = await invoke("list_backup_history");
    $("bHist").innerHTML = b.map((r) => `<tr>
      <td>${fmtWhen(r.ts)}</td><td>${r.dbname || ""}</td><td>${r.telemetry || ""}</td>
      <td class="size">${r.status === "success" ? humanBytes(r.archive_bytes) : ""}</td>
      <td>${statusBadge(r.status)}</td>
      <td class="cat-note" title="${r.message || r.output || ""}">${baseName(r.output) || (r.message || "")}</td></tr>`).join("")
      || `<tr><td colspan="6" class="cat-note">No backups yet.</td></tr>`;
    const rr = await invoke("list_restore_history");
    $("rHist").innerHTML = rr.map((r) => `<tr>
      <td>${fmtWhen(r.ts)}</td><td>${r.dbname || ""}</td><td>${statusBadge(r.status)}</td>
      <td class="cat-note" title="${r.message || r.input || ""}">${baseName(r.input)}</td></tr>`).join("")
      || `<tr><td colspan="4" class="cat-note">No restores yet.</td></tr>`;
  } catch (e) { setStatus("History error: " + e, true); }
}

async function clearHistory() {
  try { await invoke("clear_history"); await loadHistory(); } catch (e) { setStatus("Clear failed: " + e, true); }
}

// ---- Backup ------------------------------------------------------------------
function skipList() {
  const skip = [];
  document.querySelectorAll("input.cat[type=checkbox]").forEach((cb) => {
    if (!cb.checked && !cb.disabled) skip.push(cb.dataset.cid);
  });
  return skip;
}

async function backup() {
  const skip = skipList();
  const teleIncluded = !skip.includes("telemetry_historical");
  const mode = document.querySelector('input[name="teleMode"]:checked')?.value;
  const telemetryDays = teleIncluded && mode === "days" ? Number($("teleDays").value) : null;

  let passphrase = null;
  if ($("encryptChk").checked) {
    const p1 = $("encPass").value, p2 = $("encPass2").value;
    if (!p1) { $("encHint").textContent = "Enter a password."; return; }
    if (p1 !== p2) { $("encHint").textContent = "Passwords don't match."; return; }
    passphrase = p1;
  }
  $("encHint").textContent = "";

  const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "");
  const defaultName = `${$("dbname").value}-${stamp}.sentient-backup`;
  const output = await invoke("pick_save_path", { defaultName });
  if (!output) return; // cancelled

  $("backupBtn").disabled = true;
  $("backupStatus").textContent = "";
  backupProgress.start();
  try {
    const res = await invoke("backup", {
      ...conn(),
      output,
      skip,
      telemetryDays,
      fileStores: [],
      passphrase,
      onProgress: backupProgress.channel(),
    });
    backupProgress.succeed();
    $("backupStatus").textContent = `Done — ${humanBytes(res.archive_bytes)} → ${res.output}`;
  } catch (e) {
    backupProgress.fail();
    $("backupStatus").textContent = "Backup failed: " + e;
  } finally {
    $("backupBtn").disabled = false;
  }
}

// ---- Restore -----------------------------------------------------------------
async function createDb() {
  const name = $("newDbName").value.trim();
  if (!name) { $("createDbStatus").textContent = "Enter a name."; return; }
  $("createDbBtn").disabled = true;
  $("createDbStatus").textContent = "Creating…";
  try {
    await invoke("create_database", { ...conn(), name });
    $("dbname").value = name;
    await connect();          // connect + inspect the (empty) new DB
    showView("restore");      // stay on the restore tab
    $("createDbStatus").textContent = `Created '${name}' — now choose a file and Restore.`;
  } catch (e) {
    $("createDbStatus").textContent = "Failed: " + e;
  } finally {
    $("createDbBtn").disabled = false;
  }
}

async function pickRestoreFile() {
  const p = await invoke("pick_open_path");
  if (!p) return;
  restoreFile = p;
  $("pickedName").textContent = p.split(/[\\/]/).pop();
  $("restoreBtn").disabled = false;
  try {
    const enc = await invoke("is_encrypted", { path: p });
    $("restorePassRow").style.display = enc ? "" : "none";
    if (!enc) $("restorePass").value = "";
  } catch { $("restorePassRow").style.display = "none"; }
}

async function restore() {
  if (!restoreFile) return;
  $("restoreBtn").disabled = true;
  $("pickBtn").disabled = true;
  $("restoreStatus").textContent = "";
  restoreProgress.start();
  try {
    const passphrase = $("restorePassRow").style.display !== "none" ? $("restorePass").value : null;
    const res = await invoke("restore", {
      ...conn(),
      input: restoreFile,
      allowNonempty: false,
      fileStorePaths: [],
      passphrase,
      onProgress: restoreProgress.channel(),
    });
    restoreProgress.succeed();
    $("restoreStatus").textContent = `Restored into '${res.database}'.`;
  } catch (e) {
    restoreProgress.fail();
    $("restoreStatus").textContent = "Restore failed: " + e;
  } finally {
    $("restoreBtn").disabled = false;
    $("pickBtn").disabled = false;
  }
}

// ---- Init + wiring -----------------------------------------------------------
function toggleEncrypt() {
  const on = $("encryptChk").checked;
  $("encryptFields").style.display = on ? "" : "none";
  try { invoke("setting_set", { key: "encrypt_default", value: on ? "1" : "0" }); } catch { /* store off */ }
}

async function init() {
  try { HOST = await invoke("host_os"); } catch { /* pre-host_os backend: assume Windows */ }
  renderModes();
  renderConfigForm();
  initTheme();
  updateGating();
  if (!invoke) return;
  // An offline build ships its archives in a `payload` folder beside the exe.
  // Resolve it once so the wizard can choose Bundled over Remote without the
  // operator needing to know which build they were handed.
  try { BUNDLE_DIR = await invoke("native_bundle_dir"); } catch { BUNDLE_DIR = null; }
  await loadProfiles();
  try {
    const last = await invoke("setting_get", { key: "last_conn" });
    if (last) {
      const c = JSON.parse(last);
      if (c.host) $("host").value = c.host;
      if (c.port) $("port").value = c.port;
      if (c.dbname) $("dbname").value = c.dbname;
      if (c.user) $("user").value = c.user;
    }
    if ((await invoke("setting_get", { key: "encrypt_default" })) === "1") {
      $("encryptChk").checked = true;
      $("encryptFields").style.display = "";
    }
    // Restore the last password and quietly reconnect (no view switch, no error
    // banner if it can't reach the DB right now).
    try {
      const pw = await invoke("get_last_password");
      if (pw) $("password").value = pw;
    } catch { /* keychain unavailable */ }
    if ($("host").value && $("password").value) {
      connect(false, true);
    }
  } catch { /* no saved settings */ }
}

$("themeToggle").addEventListener("click", toggleTheme);
$("connect").addEventListener("click", () => connect());
$("backupBtn").addEventListener("click", backup);
$("createDbBtn").addEventListener("click", createDb);
$("pickBtn").addEventListener("click", pickRestoreFile);
$("restoreBtn").addEventListener("click", restore);
$("profiles").addEventListener("change", onProfileSelect);
$("saveProfileBtn").addEventListener("click", saveProfile);
$("deleteProfileBtn").addEventListener("click", deleteProfile);
$("clearHistoryBtn").addEventListener("click", clearHistory);
$("encryptChk").addEventListener("change", toggleEncrypt);
document.querySelectorAll(".sidebar .nav").forEach((b) =>
  b.addEventListener("click", () => showView(b.dataset.view))
);
document.querySelectorAll('input[name="themeMode"]').forEach((r) =>
  r.addEventListener("change", () => applyThemeMode(r.value))
);
init();

// ===========================================================================
// Setup wizard: System check → Components → Configure → Install (automated).
// The Install step drives WSL2 → Docker → deploy back-to-back, survives the
// WSL reboot (resumes on relaunch), and can be cancelled (with confirmation)
// then cleaned up. Shares invoke / Channel / $ defined above.
// ===========================================================================
const STEPS = ["checks", "components", "configure", "install"];
const CHECK_ICON = { pass: "i-pass", setup: "i-todo", fail: "i-fail", unknown: "i-unknown" };
let installing = false; // guard against double-starts

function showStep(name) {
  document.querySelectorAll("#setupPage .chip").forEach((c) => {
    const i = STEPS.indexOf(c.dataset.step), ci = STEPS.indexOf(name);
    c.classList.toggle("active", c.dataset.step === name);
    c.classList.toggle("done", i >= 0 && ci >= 0 && i < ci);
  });
  document.querySelectorAll("#setupPage .view").forEach((v) => v.classList.remove("active"));
  const view = $(name + "View");
  if (view) view.classList.add("active");
}

// ---- Step 1: checks ----------------------------------------------------------
function renderChecks(list) {
  $("checksList").innerHTML = list
    .map(
      (c) => `
      <div class="check">
        <svg class="icon s-${c.status}"><use href="#${CHECK_ICON[c.status] || "i-unknown"}"/></svg>
        <div>
          <div class="label">${c.label}</div>
          <div class="detail">${c.detail}</div>
        </div>
      </div>`
    )
    .join("");
  const fails = list.filter((c) => c.status === "fail");
  const setups = list.filter((c) => c.status === "setup");
  const s = $("checksSummary");
  const modeTitle = (MODES[isLinux() ? "linux" : "windows"].find((m) => m.id === deployMode()) || {}).title || deployMode();
  if (fails.length) {
    // The checks depend on the mode chosen on the NEXT step, so a blocker
    // here must not trap the operator on this step: they may be about to pick
    // the other mode. The Install step re-checks the final choice and gates
    // there instead.
    s.className = "summary bad";
    s.innerHTML = `${fails.length} blocker${fails.length > 1 ? "s" : ""} for “${modeTitle}”. Resolve ${fails.length > 1 ? "them" : "it"}, or choose a different way to run SENTIENT on the next step.`;
  } else {
    s.className = "summary";
    s.textContent = setups.length
      ? `Ready for “${modeTitle}” — the installer will set up ${setups.length} item${setups.length > 1 ? "s" : ""}.`
      : `Everything's ready for “${modeTitle}”.`;
  }
  $("toComponents").disabled = false;
  return fails;
}

/// The TCP ports the chosen configuration binds, for the port check.
function configuredPorts() {
  const c = readConfig();
  const docker = deployMode() === "docker";
  const p = [[c.http_port, "HTTP"], [c.pg_port, "PostgreSQL"]];
  if (docker) p.push([c.mqtt_port, "MQTT"], [c.mqtts_port, "MQTTS"]);
  if (c.flavor === "clickhouse") p.push([c.ch_http_port, "ClickHouse HTTP"], [c.ch_native_port, "ClickHouse native"]);
  return p;
}

/// Final gate: re-run the checks for the mode actually chosen and refuse to
/// start while a blocker remains. Called whenever the Install step is shown.
async function gateInstall() {
  if (!invoke) return;
  const btn = $("startInstallBtn"), note = $("installGate");
  btn.disabled = true;
  note.textContent = "Checking this machine for the chosen mode…";
  try {
    const list = await invoke("preflight_for", { mode: preflightMode(), offline: isOfflineBundle(), ports: configuredPorts() });
    const fails = list.filter((c) => c.status === "fail");
    if (fails.length) {
      note.innerHTML = `<b style="color:var(--err)">Cannot start:</b> ` +
        fails.map((c) => `<b>${c.label}</b> — ${c.detail}`).join("<br>") +
        `<br>Fix ${fails.length > 1 ? "these" : "this"} and come back, or go back and choose a different way to run SENTIENT.`;
      return;
    }
    note.textContent = "";
    btn.disabled = false;
  } catch (e) {
    note.textContent = "Check failed: " + e;
  }
}

async function recheck() {
  if (!invoke) return;
  $("recheckBtn").disabled = true;
  $("checksList").innerHTML =
    '<div class="check"><svg class="icon spin s-unknown"><use href="#i-spin"/></svg><div><div class="label">Checking…</div></div></div>';
  try {
    // The two modes need different prerequisites: WSL/virtualization/Docker for
    // the container mode, free ports and no competing PostgreSQL for native.
    renderChecks(await invoke("preflight_for", { mode: preflightMode(), offline: isOfflineBundle() }));
  } catch (e) {
    $("checksSummary").className = "summary bad";
    $("checksSummary").textContent = "Check failed: " + e;
  } finally {
    $("recheckBtn").disabled = false;
  }
}

// ---- Step 3: configure -------------------------------------------------------
/// The mode SENTIENT was actually installed with, as opposed to whatever the
/// Components radio currently shows. Cached after the first lookup.
///
/// The stored setting is only written by an install that ran through this
/// wizard, so it is absent for perfectly ordinary cases: an upgrade from a
/// build that predates it, a stack provisioned by hand, or the headless
/// driver. Rather than guessing, ask the machine — if the native services are
/// registered, it is a native install. Docker remains the fallback, since
/// every install predating this setting was one.
let _installedMode = null;
async function installedMode() {
  if (_installedMode) return _installedMode;
  try {
    const v = await invoke("setting_get", { key: "deploy_mode" });
    if (v === "native" || v === "docker") { _installedMode = v; return v; }
  } catch { /* store off */ }
  try {
    const st = await invoke("native_status", { options: readNativeOptions() });
    if (st && st.installed) {
      _installedMode = "native";
      // Remember it, so the probe only happens once per machine.
      try { await invoke("setting_set", { key: "deploy_mode", value: "native" }); } catch { /* store off */ }
      return _installedMode;
    }
  } catch { /* native engine unavailable */ }
  _installedMode = "docker";
  return _installedMode;
}

/// What the wizard is running on — filled from the backend at start-up.
let HOST = { os: "windows", arch: "x86_64" };
function isLinux() { return HOST.os === "linux"; }

/// Which deployment mode the operator picked on the Components step.
function deployMode() {
  const el = document.querySelector('input[name="deployMode"]:checked');
  return el ? el.value : (isLinux() ? "docker" : "native");
}

/// Time-series storage flavour chosen on the Components step. The Windows
/// native tier is TimescaleDB (the bundled PostgreSQL ships it); everywhere
/// else the reference hybrid is the default.
function flavor() {
  if (!isLinux() && deployMode() === "native") return "timescaledb";
  const el = document.querySelector('input[name="flavor"]:checked');
  return el ? el.value : "clickhouse";
}
function flavorLabel() {
  return (FLAVORS.find((f) => f.id === flavor()) || {}).title || "PostgreSQL";
}

/// The preflight list is keyed by host OS *and* mode, so a Linux machine is
/// never asked about a Windows build number.
function preflightMode() {
  return `${isLinux() ? "linux" : "windows"}-${deployMode()}`;
}

const MODES = {
  windows: [
    { id: "native", title: "Directly on Windows", badge: "recommended", checked: true,
      desc: "Installs PostgreSQL and SENTIENT as Windows services. No WSL2, no Docker, fewer moving parts, and it works on machines with no internet access." },
    { id: "docker", title: "In Docker (WSL2)", badge: "",
      desc: "Runs the stack in containers inside WSL2. Choose this if you already standardise on Docker, or want the container images rather than native services." },
  ],
  linux: [
    { id: "docker", title: "In Docker on this machine", badge: "recommended", checked: true,
      desc: "Runs SENTIENT's own compose stack with the Docker already on this machine (installed from your distribution if missing). The same containers the Linux release ships as; upgrades are an image pull." },
    { id: "native", title: "Natively with systemd", badge: "",
      desc: "PostgreSQL (and ClickHouse) from your distribution's packages and SENTIENT as a systemd service, no containers. For machines that don't run Docker." },
  ],
};

const FLAVORS = [
  { id: "clickhouse", title: "PostgreSQL + ClickHouse", badge: "recommended", checked: true,
    desc: "Entities in PostgreSQL, telemetry history in ClickHouse. Handles the highest write rates and the longest retention; what the reference deployment runs." },
  { id: "timescaledb", title: "PostgreSQL + TimescaleDB", badge: "",
    desc: "Everything in one PostgreSQL with TimescaleDB hypertables. One database to back up; the tier the Windows native install uses." },
  { id: "stock", title: "PostgreSQL only", badge: "",
    desc: "Plain PostgreSQL. Smallest footprint, for small sites and single-board computers." },
];

function optionHtml(name, o) {
  return `<label class="comp" style="color:inherit" data-opt="${o.id}">
    <input type="radio" name="${name}" id="${name}-${o.id}" value="${o.id}" ${o.checked ? "checked" : ""} />
    <span><span class="ct">${o.title}</span> ${o.badge ? `<span class="badge">${o.badge}</span>` : ""}
      <span class="cd">${o.desc}</span></span></label>`;
}

function renderModes() {
  const card = document.getElementById("modeCard");
  if (!card) return;
  const list = MODES[isLinux() ? "linux" : "windows"];
  card.innerHTML = list.map((o) => optionHtml("deployMode", o)).join("");
  card.querySelectorAll('input[name="deployMode"]').forEach((el) =>
    el.addEventListener("change", () => { renderFlavors(); refreshCfgVisibility(); recheck(); }));
  renderFlavors();
}

/// The flavour choice applies wherever the compose/systemd engines can honour
/// it: every Docker install, and Linux native (minus TimescaleDB, which no
/// distribution packages). Windows native is fixed to TimescaleDB.
function renderFlavors() {
  const block = document.getElementById("flavorBlock");
  const card = document.getElementById("flavorCard");
  if (!block || !card) return;
  const mode = deployMode();
  const show = isLinux() || mode === "docker";
  block.hidden = !show;
  if (!card.children.length) {
    card.innerHTML = FLAVORS.map((o) => optionHtml("flavor", o)).join("");
    card.querySelectorAll('input[name="flavor"]').forEach((el) =>
      el.addEventListener("change", () => { refreshCfgVisibility(); updatePlatformDesc(); }));
  }
  const tsdb = card.querySelector('[data-opt="timescaledb"]');
  const tsdbHidden = isLinux() && mode === "native";
  tsdb.hidden = tsdbHidden;
  if (tsdbHidden && tsdb.querySelector("input").checked) card.querySelector("#flavor-clickhouse").checked = true;
  updatePlatformDesc();
}

function updatePlatformDesc() {
  const el = document.getElementById("platformDesc");
  if (el) el.textContent = `The server and web UI, with ${flavorLabel()} as its storage.`;
}

function isOfflineBundle() {
  return Boolean(BUNDLE_DIR);
}

/// The native engine takes the same stack configuration as the Docker one,
/// plus where to get its archives from.
function readNativeOptions() {
  const o = { ...readConfig() };
  if (BUNDLE_DIR) o.bundle_dir = BUNDLE_DIR;
  return o;
}

// ---- configuration model -------------------------------------------------------
// One table drives the form, the defaults, the summary and validation. Keys
// are the snake_case names the backend's DeployConfig deserialises; `modes`
// and `flavors` limit a field to where the engine can honour it.
const CFG_GROUPS = [
  { id: "basics", basics: true, fields: [
    { key: "db_name", label: "Database name", type: "text", def: "sentient" },
    { key: "db_user", label: "Database user", type: "text", def: "sentient" },
    { key: "db_password", label: "Database password", type: "text", def: "sentient" },
    { key: "http_port", label: "Web port (HTTP)", type: "number", def: 8080 },
    { key: "mqtt_port", label: "MQTT port", type: "number", def: 1883, modes: ["docker"] },
    { key: "coap_port", label: "CoAP port (UDP)", type: "number", def: 5683, modes: ["docker"] },
    { key: "load_demo", label: "Load demo data", type: "checkbox", def: false, badge: "optional",
      hint: "Seed demo tenants, devices and dashboards so there's something to explore. Leave off for a clean install." },
  ]},
  { id: "identity", title: "Identity & licensing", hint: "Two nodes reporting the same service id are treated as one node.", fields: [
    { key: "instance_id", label: "Instance id", type: "text", def: () => deployMode() === "docker" ? "sentient-docker-01" : "sentient-01" },
    { key: "service_id", label: "Service id", type: "text", def: "st-node-1" },
    { key: "license_server_url", label: "Licence server URL", type: "text", def: "https://license.invenia.in" },
  ]},
  { id: "stack", title: "Containers & image", modes: ["docker"], fields: [
    { key: "stack_name", label: "Stack name", type: "text", def: "sentient", fh: "Prefix for container and volume names; change it to run a second stack beside an existing one." },
    { key: "image", label: "SENTIENT image", type: "text", def: "quakestring/sentient:latest", fh: "Offline sites point this at the tag loaded with docker load." },
    { key: "log_max_size", label: "Container log size", type: "text", def: "100m" },
    { key: "log_max_file", label: "Container log files kept", type: "number", def: 10 },
  ]},
  { id: "ports", title: "More ports", fields: [
    { key: "pg_port", label: "PostgreSQL port", type: "number", def: 5432 },
    { key: "mqtts_port", label: "MQTTS port", type: "number", def: 8883, modes: ["docker"] },
    { key: "coaps_port", label: "CoAPS port (UDP)", type: "number", def: 5684, modes: ["docker"] },
    { key: "ch_http_port", label: "ClickHouse HTTP port", type: "number", def: 8123, flavors: ["clickhouse"] },
    { key: "ch_native_port", label: "ClickHouse native port", type: "number", def: 9000, flavors: ["clickhouse"] },
  ]},
  { id: "pg", title: "PostgreSQL server", hint: "Applied at start. Sizes use PostgreSQL units: 128MB, 2GB.", fields: [
    { key: "pg_max_connections", label: "max_connections", type: "number", def: 100 },
    { key: "pg_shared_buffers", label: "shared_buffers", type: "text", def: "128MB" },
    { key: "pg_work_mem", label: "work_mem", type: "text", def: "2MB" },
    { key: "pg_effective_cache_size", label: "effective_cache_size", type: "text", def: "384MB" },
  ]},
  { id: "ch", title: "ClickHouse", flavors: ["clickhouse"], hint: "ClickHouse's own defaults assume a dedicated server; these assume the whole stack shares one modest machine.", fields: [
    { key: "ch_db", label: "ClickHouse database", type: "text", def: "sentient" },
    { key: "ch_user", label: "ClickHouse user", type: "text", def: "sentient" },
    { key: "ch_password", label: "ClickHouse password", type: "text", def: "sentient" },
    { key: "ch_mark_cache_bytes", label: "Mark cache (bytes)", type: "number", def: 268435456 },
    { key: "ch_uncompressed_cache_bytes", label: "Uncompressed cache (bytes)", type: "number", def: 134217728 },
    { key: "ts_latest_cache_max_entries", label: "Latest-values cache entries", type: "number", def: 1000000, fh: "In-process cache of the latest value per key; entries live until evicted by size." },
  ]},
  { id: "pools", title: "Connection pools", hint: "Primary pool serves requests; the secondary pool serves background writers.", fields: [
    { key: "db_pool_primary_min", label: "Primary pool min", type: "number", def: 16 },
    { key: "db_pool_primary_max", label: "Primary pool max", type: "number", def: 48 },
    { key: "database_pool_min", label: "Secondary pool min", type: "number", def: 8 },
    { key: "database_pool_max", label: "Secondary pool max", type: "number", def: 24 },
  ]},
  { id: "batch", title: "Write batching", hint: "Rows per batch, writer threads, and the longest a row waits (ms) before a partial batch is flushed.", fields: [
    { key: "sql_ts_batch_size", label: "Telemetry batch size", type: "number", def: 8000 },
    { key: "sql_ts_batch_threads", label: "Telemetry threads", type: "number", def: 5 },
    { key: "sql_ts_batch_max_delay_ms", label: "Telemetry max delay (ms)", type: "number", def: 100 },
    { key: "sql_ts_latest_batch_size", label: "Latest-values batch size", type: "number", def: 4000 },
    { key: "sql_ts_latest_batch_threads", label: "Latest-values threads", type: "number", def: 5 },
    { key: "sql_ts_latest_batch_max_delay_ms", label: "Latest-values max delay (ms)", type: "number", def: 50 },
    { key: "sql_attributes_batch_size", label: "Attributes batch size", type: "number", def: 4000 },
    { key: "sql_attributes_batch_threads", label: "Attributes threads", type: "number", def: 5 },
    { key: "sql_attributes_batch_max_delay_ms", label: "Attributes max delay (ms)", type: "number", def: 50 },
  ]},
  { id: "caches", title: "Entity caches", hint: "Maximum entries and time-to-live in minutes.", fields: [
    { key: "cache_devices_max_size", label: "Devices: max entries", type: "number", def: 100000 },
    { key: "cache_devices_ttl", label: "Devices: TTL (min)", type: "number", def: 1440 },
    { key: "cache_devices_null_ttl", label: "Devices: not-found TTL (min)", type: "number", def: 5 },
    { key: "cache_device_profiles_max_size", label: "Device profiles: max entries", type: "number", def: 10000 },
    { key: "cache_device_profiles_ttl", label: "Device profiles: TTL (min)", type: "number", def: 1440 },
    { key: "cache_asset_profiles_max_size", label: "Asset profiles: max entries", type: "number", def: 10000 },
    { key: "cache_asset_profiles_ttl", label: "Asset profiles: TTL (min)", type: "number", def: 1440 },
  ]},
  { id: "retention", title: "Retention & cleanup", hint: "0 keeps forever.", fields: [
    { key: "audit_log_ttl_days", label: "Audit log TTL (days)", type: "number", def: 0 },
    { key: "notifications_ttl_days", label: "Notifications TTL (days)", type: "number", def: 0 },
    { key: "event_cleanup_interval_secs", label: "Event cleanup interval (s)", type: "number", def: 3600 },
    { key: "events_ttl_secs", label: "Events TTL (s)", type: "number", def: 0 },
    { key: "debug_events_ttl_secs", label: "Debug events TTL (s)", type: "number", def: 604800 },
    { key: "event_cleanup_enabled", label: "Event cleanup enabled", type: "checkbox", def: true, hint: "Periodically purge expired events, audit-log rows and finished jobs." },
  ]},
  { id: "logging", title: "Logging & extra environment", fields: [
    { key: "rust_log", label: "RUST_LOG", type: "text", def: "info,sentient_api=info" },
    { key: "analytics_enabled", label: "Analytics enabled", type: "checkbox", def: true, hint: "Run the batch analytics / event pipelines." },
    { key: "extra_env", label: "Extra environment — NAME=value, one per line", type: "textarea", def: [], fh: "Appended last, so it overrides any setting above." },
  ]},
];
const CFG_FIELDS = CFG_GROUPS.flatMap((g) => g.fields.map((f) => ({ ...f, group: g })));
const fieldDef = (f) => (typeof f.def === "function" ? f.def() : f.def);
function cfgDefaults() {
  const d = { flavor: flavor() };
  for (const f of CFG_FIELDS) d[f.key] = fieldDef(f);
  return d;
}
function fieldVisible(f) {
  const g = f.group || {};
  const ok = (o) => (!o.modes || o.modes.includes(deployMode())) && (!o.flavors || o.flavors.includes(flavor()));
  return ok(g) && ok(f);
}

function fieldHtml(f) {
  const id = `cfg_${f.key}`;
  if (f.type === "checkbox") {
    return `<label class="comp" style="color:inherit" data-key="${f.key}">
      <input type="checkbox" id="${id}" />
      <span><span class="ct">${f.label}</span> ${f.badge ? `<span class="badge">${f.badge}</span>` : ""}
        ${f.hint ? `<span class="cd">${f.hint}</span>` : ""}</span></label>`;
  }
  const help = f.fh ? `<span class="fh">${f.fh}</span>` : "";
  if (f.type === "textarea") {
    return `<label class="wide" data-key="${f.key}">${f.label}<textarea id="${id}" spellcheck="false"></textarea>${help}</label>`;
  }
  return `<label data-key="${f.key}">${f.label}<input id="${id}" type="${f.type === "number" ? "number" : "text"}" />${help}</label>`;
}

/// One group's fields: the value fields on a grid, the on/off ones listed
/// under it (a checkbox in a grid cell leaves a ragged hole).
function groupBodyHtml(fields) {
  const boxes = fields.filter((f) => f.type === "checkbox");
  const rest = fields.filter((f) => f.type !== "checkbox");
  return (rest.length ? `<div class="fieldgrid">${rest.map(fieldHtml).join("")}</div>` : "")
    + (boxes.length ? `<div class="checkgrid">${boxes.map(fieldHtml).join("")}</div>` : "");
}

/// Build the form once; visibility is refreshed as the mode/flavour change.
let _cfgRendered = false;
function renderConfigForm() {
  if (_cfgRendered) return;
  _cfgRendered = true;
  const basics = CFG_GROUPS.find((g) => g.basics);
  $("cfgBasics").innerHTML = groupBodyHtml(basics.fields);
  $("cfgGroups").innerHTML = CFG_GROUPS.filter((g) => !g.basics).map((g) =>
    `<details class="cfg-group" data-group="${g.id}">
      <summary>${g.title}<span class="gh" id="gh_${g.id}"></span></summary>
      <div class="body">
        ${g.hint ? `<p class="hint">${g.hint}</p>` : ""}
        ${groupBodyHtml(g.fields)}
      </div>
    </details>`).join("");
  document.querySelectorAll("#customCard input, #customCard textarea").forEach((el) =>
    el.addEventListener("input", () => { updateGroupBadges(); $("cfgError").textContent = ""; }));
  applyConfig(cfgDefaults());
  refreshCfgVisibility();
}

function refreshCfgVisibility() {
  if (!_cfgRendered) return;
  for (const f of CFG_FIELDS) {
    const el = document.querySelector(`#customCard [data-key="${f.key}"]`);
    if (el) el.hidden = !fieldVisible(f);
  }
  for (const g of CFG_GROUPS.filter((g) => !g.basics)) {
    const el = document.querySelector(`#customCard [data-group="${g.id}"]`);
    if (el) el.hidden = !g.fields.some((f) => fieldVisible({ ...f, group: g }));
  }
  updateGroupBadges();
}

function changedFields(c = readConfig()) {
  const d = cfgDefaults();
  return CFG_FIELDS.filter((f) => fieldVisible(f) && JSON.stringify(c[f.key]) !== JSON.stringify(d[f.key]));
}
function updateGroupBadges() {
  const changed = changedFields();
  for (const g of CFG_GROUPS.filter((g) => !g.basics)) {
    const n = changed.filter((f) => f.group.id === g.id).length;
    const el = $(`gh_${g.id}`);
    if (el) el.textContent = n ? `${n} changed` : "";
  }
}

function parseExtraEnv(text) {
  return String(text || "").split(/\r?\n/).map((l) => l.trim()).filter((l) => l && !l.startsWith("#"))
    .map((l) => { const i = l.indexOf("="); return i < 0 ? { name: l, value: "" } : { name: l.slice(0, i).trim(), value: l.slice(i + 1).trim() }; });
}

function readConfig() {
  const c = { flavor: flavor() };
  for (const f of CFG_FIELDS) {
    const el = $(`cfg_${f.key}`);
    const d = fieldDef(f);
    if (!el) { c[f.key] = d; continue; }
    if (f.type === "checkbox") c[f.key] = el.checked;
    else if (f.type === "number") { const v = parseInt(el.value, 10); c[f.key] = Number.isFinite(v) ? v : d; }
    else if (f.type === "textarea") c[f.key] = parseExtraEnv(el.value);
    else c[f.key] = (el.value || "").trim() || d;
  }
  return c;
}
function applyConfig(c) {
  for (const f of CFG_FIELDS) {
    const el = $(`cfg_${f.key}`);
    if (!el) continue;
    const v = c[f.key] !== undefined ? c[f.key] : fieldDef(f);
    if (f.type === "checkbox") el.checked = Boolean(v);
    else if (f.type === "textarea") el.value = (Array.isArray(v) ? v : []).map((e) => `${e.name}=${e.value}`).join("\n");
    else el.value = v;
  }
  updateGroupBadges();
}
function renderReco() {
  const c = readConfig();
  const docker = deployMode() === "docker";
  const rows = [
    ["Runs", (MODES[isLinux() ? "linux" : "windows"].find((m) => m.id === deployMode()) || {}).title || deployMode()],
    ["Storage", flavorLabel()],
    ["Database", `${c.db_name} (user ${c.db_user}${c.pg_port !== 5432 ? `, port ${c.pg_port}` : ""})`],
    ["Web address", `http://localhost:${c.http_port}`],
  ];
  if (docker) rows.push(["MQTT / CoAP ports", `${c.mqtt_port} / ${c.coap_port}`]);
  rows.push(["Demo data", c.load_demo ? "Yes" : "No (clean install)"]);
  rows.push(["Kiosk launcher", $("compKiosk").checked ? (isLinux() ? "Desktop launcher" : "Desktop shortcut (Chrome if needed)") : "Skip"]);
  const changed = changedFields(c).filter((f) => !f.group.basics);
  rows.push(["Advanced", changed.length
    ? `${changed.length} changed — ${changed.slice(0, 6).map((f) => f.label).join(", ")}${changed.length > 6 ? ", …" : ""}`
    : "Reference defaults"]);
  $("recoSummary").innerHTML = rows
    .map(([k, v]) => `<tr><td class="cat-note" style="width:42%">${k}</td><td>${v}</td></tr>`)
    .join("");
}
function validateConfig(c) {
  const ident = /^[A-Za-z0-9_]+$/;
  const badPw = /[\s'"\\@/:#?&$`]/;
  const docker = deployMode() === "docker";
  const ch = c.flavor === "clickhouse";
  for (const [n, v] of [["Database name", c.db_name], ["Database user", c.db_user]])
    if (!ident.test(v)) return `${n} may only contain letters, digits and underscores.`;
  if (badPw.test(c.db_password)) return "Database password may not contain spaces, quotes or @ / : # ? & $ ` \\.";
  if (docker && !ident.test(c.stack_name)) return "Stack name may only contain letters, digits and underscores.";
  if (docker && /\s/.test(c.image)) return "Image must be a Docker image reference like quakestring/sentient:latest.";
  if (ch) {
    for (const [n, v] of [["ClickHouse database", c.ch_db], ["ClickHouse user", c.ch_user]])
      if (!ident.test(v)) return `${n} may only contain letters, digits and underscores.`;
    if (badPw.test(c.ch_password)) return "ClickHouse password may not contain spaces, quotes or @ / : # ? & $ ` \\.";
  }
  if (!/^https?:\/\/\S+$/.test(c.license_server_url)) return "Licence server URL must start with http:// or https://.";
  const ports = [["Web", c.http_port], ["PostgreSQL", c.pg_port]];
  if (docker) ports.push(["MQTT", c.mqtt_port], ["MQTTS", c.mqtts_port], ["CoAP", c.coap_port], ["CoAPS", c.coaps_port]);
  if (ch) ports.push(["ClickHouse HTTP", c.ch_http_port], ["ClickHouse native", c.ch_native_port]);
  for (const [n, p] of ports) if (!(p >= 1 && p <= 65535)) return `${n} port must be between 1 and 65535.`;
  if (new Set(ports.map(([, p]) => p)).size !== ports.length) return "Every port must be different.";
  for (const f of CFG_FIELDS)
    if (f.type === "number" && fieldVisible(f) && !(c[f.key] >= 0)) return `${f.label} must be a non-negative number.`;
  for (const e of c.extra_env) if (!ident.test(e.name)) return `Extra variable "${e.name}" is not a valid environment variable name.`;
  return null;
}
async function persistConfig() {
  try { await invoke("setting_set", { key: "install_config", value: JSON.stringify(readConfig()) }); } catch { /* store off */ }
}
async function loadConfig() {
  renderConfigForm();
  try {
    const s = await invoke("setting_get", { key: "install_config" });
    if (s) {
      const saved = JSON.parse(s);
      applyConfig({ ...cfgDefaults(), ...saved });
      const fl = document.getElementById(`flavor-${saved.flavor}`);
      if (fl && !fl.closest("label").hidden) fl.checked = true;
    }
  } catch { /* no saved config */ }
  refreshCfgVisibility();
  renderReco();
}

/// Mode-specific copy for the Install step, set when the step is entered.
function renderInstallPlan() {
  const docker = deployMode() === "docker";
  let plan, sub;
  if (docker && !isLinux()) {
    plan = "Ready to set up <b>WSL2</b>, <b>Docker Engine</b>, and deploy the <b>SENTIENT</b> stack. This can take several minutes and may restart your PC once.";
    sub = "This sets up WSL2, Docker Engine, and deploys SENTIENT automatically. It may restart your PC once — it'll resume on its own.";
  } else if (docker) {
    plan = `Ready to make sure <b>Docker</b> is available, then deploy the <b>SENTIENT</b> stack (${flavorLabel()}). Pulling the images takes a few minutes the first time. You may be asked for your password if Docker has to be installed.`;
    sub = "This installs Docker from your distribution if needed and deploys SENTIENT's compose stack.";
  } else if (isLinux()) {
    plan = `Ready to install <b>${flavorLabel()}</b> from your distribution's packages and set up <b>SENTIENT</b> as a systemd service. You will be asked for your password.`;
    sub = "This installs the databases from your distribution and registers SENTIENT as a systemd service.";
  } else {
    plan = "Ready to install <b>PostgreSQL + TimescaleDB</b> and set up <b>SENTIENT</b> as Windows services. No WSL2, no Docker.";
    sub = "This installs PostgreSQL and SENTIENT as Windows services.";
  }
  $("installPlan").innerHTML = plan;
  $("installSub").textContent = sub;
}

/// Wording that depends on the host OS and the installed mode.
async function applyOsTexts() {
  const about = $("aboutText");
  if (about) {
    about.textContent = isLinux()
      ? "SENTIENT Platform Manager — one app to install, run, update, and back up the SENTIENT platform. Deploys the stack with Docker or as systemd services; backs up and restores the database with selective components and password-lock encryption."
      : "SENTIENT Platform Manager — one app to install, run, update, and back up the SENTIENT platform. Installs SENTIENT as Windows services or in Docker (WSL2); backs up and restores the database with selective components and password-lock encryption.";
  }
  const mode = _installedMode || deployMode();
  const docker = mode === "docker";
  $("containersHead").textContent = docker ? "Container" : "Service";
  $("statusSub").textContent = docker
    ? (isLinux() ? "The SENTIENT compose stack running in Docker on this machine." : "The SENTIENT stack running in the local WSL distro.")
    : (isLinux() ? "The SENTIENT systemd services on this machine." : "The SENTIENT Windows services on this machine.");
  $("uninstallNote").innerHTML = docker
    ? (isLinux()
      ? "Permanently removes the stack's containers, images, the database and <b>all its data</b> (volumes), the desktop launcher and autostart. Docker itself and this Manager app are kept."
      : "Permanently removes the stack, the database and <b>all its data</b>, the WSL distro, the autostart entry and the desktop shortcut. WSL2 and this Manager app are kept.")
    : (isLinux()
      ? "Permanently removes the SENTIENT service and program files, its database and <b>all its data</b>, and the desktop launcher. The PostgreSQL/ClickHouse packages and this Manager app are kept."
      : "Permanently removes the SENTIENT and PostgreSQL Windows services, the program files, the database and <b>all its data</b>, the autostart entry and the desktop shortcut. This Manager app is kept.");
  $("updateHint").textContent = docker
    ? "This pulls a newer SENTIENT image from the registry. Your data is kept; the database schema is migrated if the new version needs it. The server is briefly stopped during migration."
    : "Native installs are updated by running a newer Platform Manager bundle, which replaces the program files in place and lets the server migrate the database at start.";
  $("updateBtn").style.display = docker ? "" : "none";
}

// ---- Step 4: install (automated WSL2 → Docker → deploy) ----------------------
function installCard(which) {
  for (const id of ["installStart", "installProgress", "installReboot", "installDone", "installFailed"]) {
    $(id).style.display = id === which ? "" : "none";
  }
}
function instMsg(p) {
  const bar = $("instPbar"), fill = bar.querySelector(".fill");
  if (p.type === "step") { $("instStep").textContent = p.name; bar.classList.add("run"); fill.style.width = "35%"; }
  else if (p.type === "percent") { bar.classList.remove("run"); fill.style.width = Math.round(p.value * 100) + "%"; }
  else if (p.type === "log") logAppend($("instLog"), p.line);
  else if (p.type === "done") { $("instStep").textContent = "✓ " + p.message; }
  else if (p.type === "error") { $("instStep").textContent = "✗ " + p.message; }
}
function instChannel() { const ch = new Channel(); ch.onmessage = instMsg; return ch; }
function setPhase(n, total, label) {
  $("instPhase").textContent = `Step ${n} of ${total} · ${label}`;
  $("instLog").textContent = "";
}

/// Point the Backup tab at the freshly installed local DB and connect quietly,
/// so "Connection" is ready without the operator retyping anything.
async function pointBackupTabAtDb(c) {
  try {
    $("host").value = "localhost";
    $("port").value = c.pg_port;
    $("dbname").value = c.db_name;
    $("user").value = c.db_user;
    $("password").value = c.db_password;
    await invoke("setting_set", {
      key: "last_conn",
      value: JSON.stringify({ host: "localhost", port: c.pg_port, dbname: c.db_name, user: c.db_user }),
    });
    await invoke("set_last_password", { password: c.db_password });
    connect(false, true); // quiet, no view switch
  } catch { /* non-fatal */ }
}

async function autoInstall() {
  if (installing || !invoke) return;
  installing = true;
  installCard("installProgress");
  $("cancelInstallBtn").disabled = false;
  const kiosk = $("compKiosk").checked;
  const mode = deployMode();
  const c = readConfig();

  // The phases differ by mode and OS: Windows Docker needs WSL2 first, Linux
  // Docker just needs Docker, native needs the databases then the platform.
  const phases = [];
  if (mode === "native") {
    phases.push({ label: `Databases: ${flavorLabel()}`, run: async () => {
      if (isOfflineBundle()) await invoke("native_verify_bundle", { options: readNativeOptions() }); // fail before touching the machine
      await invoke("native_setup", { onProgress: instChannel(), options: readNativeOptions() });
    } });
    phases.push({ label: "Install SENTIENT", run: async () => {
      await invoke("native_deploy", { onProgress: instChannel(), options: readNativeOptions() });
      await invoke("set_state", { step: "deployed" });
    } });
  } else {
    if (!isLinux()) {
      phases.push({ label: "WSL2", run: async () => {
        if (!(await invoke("wsl_ready"))) {
          const res = await invoke("install_wsl", { onProgress: instChannel() });
          if (res.reboot_required) {
            await invoke("set_state", { step: "wsl_pending_reboot" });
            await invoke("arm_resume");
            return "reboot";
          }
        }
        await invoke("set_state", { step: "wsl_ready" });
      } });
    }
    phases.push({ label: "Docker Engine", run: async () => {
      if (!(await invoke("docker_ready"))) await invoke("setup_docker", { onProgress: instChannel() });
      await invoke("set_state", { step: "docker_ready" });
    } });
    phases.push({ label: `Deploy SENTIENT (${flavorLabel()})`, run: async () => {
      await invoke("deploy_sentient", { onProgress: instChannel(), config: c });
      await invoke("set_state", { step: "deployed" });
    } });
  }
  if (kiosk) {
    // Non-fatal: SENTIENT is already up, so a shortcut hiccup shouldn't fail it.
    phases.push({ label: isLinux() ? "Desktop launcher" : "Desktop kiosk shortcut", optional: true, run: async () => {
      await invoke("create_kiosk_shortcut", { onProgress: instChannel(), port: c.http_port });
    } });
  }

  try {
    for (let i = 0; i < phases.length; i++) {
      const ph = phases[i];
      setPhase(i + 1, phases.length, ph.label);
      if (ph.optional) {
        try { await ph.run(); } catch (e) { instMsg({ type: "log", line: "Shortcut step skipped: " + e }); }
        continue;
      }
      const outcome = await ph.run();
      if (outcome === "reboot") {
        installing = false;
        installCard("installReboot");
        return;
      }
      if (ph.label.startsWith("Deploy") || ph.label === "Install SENTIENT") {
        // Status/Update/Uninstall run in later sessions, when the radio on the
        // Components step means nothing. Remember what was actually installed.
        try { await invoke("setting_set", { key: "deploy_mode", value: mode }); } catch { /* store off */ }
        _installedMode = mode;
        await pointBackupTabAtDb(c);
      }
    }
    installing = false;
    $("cancelInstallBtn").disabled = true;
    await applyOsTexts();
    showInstallDone();
  } catch (e) {
    installing = false;
    $("cancelInstallBtn").disabled = true;
    const msg = String(e);
    $("failMsg").textContent = /cancel/i.test(msg) ? "Install cancelled." : "Install stopped: " + msg;
    installCard("installFailed");
  }
}

function showInstallDone() {
  const url = `http://localhost:${readConfig().http_port}`;
  const auto = deployMode() === "docker" && !isLinux() ? "It starts automatically on login from now on." : "It starts automatically with the machine from now on.";
  $("doneHint").innerHTML =
    `Open <b>${url}</b> and sign in as sys-admin:<br>&nbsp;&nbsp;<b>admin@sentient.local</b> &nbsp;/&nbsp; <b>admin123</b><br>${auto}`;
  installCard("installDone");
}

async function cancelInstall() {
  const keep = deployMode() === "docker" ? (isLinux() ? "Docker" : "WSL2, Docker") : "the database packages";
  const ok = confirm(
    "Cancel the install?\n\n" +
    `The step that's running now will be stopped. This can leave half-finished pieces behind — you'll be offered a cleanup afterwards. Anything that already finished (${keep}) stays installed.`
  );
  if (!ok) return;
  $("cancelInstallBtn").disabled = true;
  $("instStep").textContent = "Cancelling…";
  try { await invoke("cancel_step"); } catch { /* ignore */ }
}

async function cleanupLeftovers() {
  const native = deployMode() === "native";
  const ok = confirm(
    "Clean up leftovers?\n\n" +
    (native
      ? "This removes the SENTIENT service, program files and its database so you can retry from a clean state. The database packages stay installed."
      : `This stops and removes the SENTIENT containers and their data volumes, and reclaims disk from partially-pulled images, so you can retry from a clean state. ${isLinux() ? "Docker stays" : "WSL2 and Docker Engine stay"} installed.`)
  );
  if (!ok) return;
  installCard("installProgress");
  setPhase(1, 1, "Cleanup");
  $("cancelInstallBtn").disabled = true;
  try {
    await (native
      ? invoke("native_cleanup", { onProgress: instChannel(), options: readNativeOptions() })
      : invoke("cleanup_install", { onProgress: instChannel(), config: readConfig() }));
    $("failMsg").textContent = "Cleanup done — ready to retry.";
  } catch (e) {
    $("failMsg").textContent = "Cleanup error: " + e;
  }
  installCard("installFailed");
}

// ---- Setup init / resume -----------------------------------------------------
async function initSetup() {
  if (!invoke) return;
  // init() resolves the host OS too, but runs concurrently; every text and
  // check below keys off it, so make sure it is known before rendering.
  try { HOST = await invoke("host_os"); } catch { /* assume Windows */ }
  try { const c = await invoke("setting_get", { key: "install_kiosk" }); if (c === "0") $("compKiosk").checked = false; } catch { /* store off */ }
  await loadConfig();
  renderInstallPlan();

  if (!invoke) return;
  let state = "checks";
  try { state = await invoke("get_state"); } catch { /* default */ }

  if (state === "deployed") {
    await installedMode();
    await applyOsTexts();
    showStep("install");
    showInstallDone();
    showView("status"); // once installed, land on Status instead of the wizard
    try { await invoke("ensure_autostart"); } catch { /* migrate old autostart task */ }
  } else if (state === "docker_ready" || state === "wsl_ready") {
    showStep("install");
    autoInstall(); // resume — readiness checks skip the finished phases
  } else if (state === "wsl_pending_reboot") {
    showStep("install");
    if (await invoke("wsl_ready").catch(() => false)) {
      await invoke("set_state", { step: "wsl_ready" });
    }
    autoInstall(); // continues the install automatically after the reboot
  } else {
    showStep("checks");
    recheck();
  }
  await applyOsTexts();
}

// ---- Setup wiring ------------------------------------------------------------
$("recheckBtn").addEventListener("click", recheck);
$("toComponents").addEventListener("click", () => showStep("components"));
$("toConfigure").addEventListener("click", () => { refreshCfgVisibility(); showCustom(false); showStep("configure"); });
$("backChecks").addEventListener("click", () => showStep("checks"));
/// Recommended ⇄ Customize. A switch, not the old one-way button: the
/// summary stays one click away, and the chosen pane is remembered while the
/// wizard is open.
function showCustom(on) {
  $("recoCard").style.display = on ? "none" : "";
  $("customCard").style.display = on ? "" : "none";
  $("segReco").classList.toggle("on", !on);
  $("segCustom").classList.toggle("on", on);
  if (!on) renderReco();
}
$("segReco").addEventListener("click", () => showCustom(false));
$("segCustom").addEventListener("click", () => showCustom(true));
$("resetDefaultsBtn").addEventListener("click", () => { applyConfig(cfgDefaults()); $("cfgError").textContent = ""; });
$("backComponents").addEventListener("click", () => showStep("components"));
$("toInstall").addEventListener("click", async () => {
  const c = readConfig();
  const err = validateConfig(c);
  if (err) { $("cfgError").textContent = err; showCustom(true); return; }
  $("cfgError").textContent = "";
  try { await invoke("setting_set", { key: "install_kiosk", value: $("compKiosk").checked ? "1" : "0" }); } catch { /* store off */ }
  await persistConfig();
  renderInstallPlan();
  showStep("install");
  installCard("installStart");
  gateInstall();
});
$("startInstallBtn").addEventListener("click", autoInstall);
$("retryInstallBtn").addEventListener("click", autoInstall);
$("rebootLater").addEventListener("click", gateInstall);
$("cancelInstallBtn").addEventListener("click", cancelInstall);
$("cleanupBtn").addEventListener("click", cleanupLeftovers);
$("rebootBtn").addEventListener("click", () => invoke("reboot_now"));
$("rebootLater").addEventListener("click", () => installCard("installStart"));
$("openBtn").addEventListener("click", () => invoke("open_sentient", { port: readConfig().http_port }));
$("backConfigure").addEventListener("click", () => showStep("configure"));
initSetup();

// ===========================================================================
// Status + Update (M3) — manage the already-deployed stack.
// ===========================================================================
function fmtState(s) {
  return s === "running"
    ? '<span style="color:var(--ok); font-weight:600">running</span>'
    : `<span style="color:var(--muted)">${s}</span>`;
}

async function loadStatus() {
  if (!invoke) return;
  let st;
  const mode = await installedMode();
  try {
    st = mode === "native"
      ? await invoke("native_status", { options: readNativeOptions() })
      : await invoke("stack_status", { config: readConfig() });
  } catch { return; }
  // The native engine reports `services`; the Docker one reports `containers`.
  // Normalise so the table below renders either without knowing which.
  st.rows = mode === "native" ? (st.services || []) : (st.containers || []);
  $("statusNotInstalled").style.display = st.installed ? "none" : "";
  $("statusBody").style.display = st.installed ? "" : "none";
  if (!st.installed) return;
  $("statusBadge").innerHTML = st.running
    ? '<span class="dot" style="background:var(--ok)"></span> Running'
    : '<span class="dot"></span> Stopped';
  $("stStartBtn").disabled = st.running;
  $("stStopBtn").disabled = !st.running;
  $("stRestartBtn").disabled = !st.running;
  $("stOpenBtn").style.display = st.running ? "" : "none";
  $("containers").innerHTML = st.rows.length
    ? st.rows.map((c) => `<tr><td>${c.name}</td><td>${fmtState(c.state)}</td><td class="cat-note">${c.status}</td></tr>`).join("")
    : `<tr><td colspan="3" class="cat-note">${mode === "native" ? "No services yet." : "No containers yet."}</td></tr>`;
}

function stMsg(p) {
  if (p.type === "step") $("stStep").textContent = p.name;
  else if (p.type === "log") logAppend($("stLog"), p.line);
  else if (p.type === "done") $("stStep").textContent = "✓ " + p.message;
  else if (p.type === "error") $("stStep").textContent = "✗ " + p.message;
}

async function stackAction(action) {
  ["stStartBtn", "stStopBtn", "stRestartBtn"].forEach((id) => ($(id).disabled = true));
  $("stProgress").style.display = "";
  $("stLog").textContent = "";
  $("stStep").textContent = "Working…";
  const ch = new Channel(); ch.onmessage = stMsg;
  try {
    await ((await installedMode()) === "native"
      ? invoke("native_control", { action, onProgress: ch, options: readNativeOptions() })
      : invoke("stack_control", { action, onProgress: ch, config: readConfig() }));
  }
  catch (e) { $("stStep").textContent = "Failed: " + e; }
  finally { await loadStatus(); }
}

async function loadLogs() {
  $("logsBox").textContent = "Loading…";
  try {
    $("logsBox").textContent =
      ((await installedMode()) === "native"
        ? await invoke("native_logs", { options: readNativeOptions(), tail: 300 })
        : await invoke("stack_logs", { tail: 300, config: readConfig() })) || "(no output)";
  }
  catch (e) { $("logsBox").textContent = "Error: " + e; }
}

async function uninstall() {
  // The two modes destroy different things, and the confirmation has to say
  // exactly which — this is the one irreversible button in the app.
  const mode = await installedMode();
  const ok = confirm("Uninstall SENTIENT?\n\n" + $("uninstallNote").textContent + " This cannot be undone.");
  if (!ok) return;
  $("uninstallBtn").disabled = true;
  $("uninProg").style.display = "";
  $("uninLog").textContent = "";
  $("uninStep").textContent = "Removing…";
  const ch = new Channel();
  ch.onmessage = (p) => {
    if (p.type === "step") $("uninStep").textContent = p.name;
    else if (p.type === "log") logAppend($("uninLog"), p.line);
    else if (p.type === "done") $("uninStep").textContent = "✓ " + p.message;
    else if (p.type === "error") $("uninStep").textContent = "✗ " + p.message;
  };
  try {
    await (mode === "native"
      ? invoke("native_cleanup", { onProgress: ch, options: readNativeOptions() })
      : invoke("uninstall_sentient", { onProgress: ch, config: readConfig() }));
    try { await invoke("setting_set", { key: "deploy_mode", value: "" }); } catch { /* store off */ }
    _installedMode = null;
    $("uninStep").textContent = "✓ SENTIENT removed.";
    await loadStatus();      // now shows "not installed"
    installCard("installStart");
    showStep("checks");      // reset the Setup wizard to a fresh state
    recheck();
  } catch (e) {
    $("uninStep").textContent = "Failed: " + e;
  } finally {
    $("uninstallBtn").disabled = false;
  }
}

async function makeShortcut() {
  $("mkShortcutBtn").disabled = true;
  $("kioskProg").style.display = "";
  $("kioskLog").textContent = "";
  $("kioskStep").textContent = "Working…";
  const ch = new Channel();
  ch.onmessage = (p) => {
    if (p.type === "step") $("kioskStep").textContent = p.name;
    else if (p.type === "log") logAppend($("kioskLog"), p.line);
    else if (p.type === "done") $("kioskStep").textContent = "✓ " + p.message;
    else if (p.type === "error") $("kioskStep").textContent = "✗ " + p.message;
  };
  try { await invoke("create_kiosk_shortcut", { onProgress: ch, port: readConfig().http_port }); }
  catch (e) { $("kioskStep").textContent = "Failed: " + e; }
  finally { $("mkShortcutBtn").disabled = false; }
}

// ---- Update ------------------------------------------------------------------
function upCard(which) {
  for (const id of ["upStart", "upProgress", "upDone", "upFailed"]) {
    $(id).style.display = id === which ? "" : "none";
  }
}
async function refreshUpdateGate() {
  if (!invoke) return;
  let st;
  const mode = await installedMode();
  try {
    st = mode === "native"
      ? await invoke("native_status", { options: readNativeOptions() })
      : await invoke("stack_status", { config: readConfig() });
  } catch { return; }
  await applyOsTexts();
  $("updateNotInstalled").style.display = st.installed ? "none" : "";
  $("updateBody").style.display = st.installed ? "" : "none";
  if (st.installed) upCard("upStart");
}
function upMsg(p) {
  const bar = $("upPbar"), fill = bar.querySelector(".fill");
  if (p.type === "step") { $("upStep").textContent = p.name; bar.classList.add("run"); fill.style.width = "35%"; }
  else if (p.type === "percent") { bar.classList.remove("run"); fill.style.width = Math.round(p.value * 100) + "%"; }
  else if (p.type === "log") logAppend($("upLog"), p.line);
  else if (p.type === "done") { $("upStep").textContent = "✓ " + p.message; }
  else if (p.type === "error") { $("upStep").textContent = "✗ " + p.message; }
}
async function runUpdate() {
  upCard("upProgress");
  $("upLog").textContent = "";
  $("upStep").textContent = "Starting…";
  $("upCancelBtn").disabled = false;
  const ch = new Channel(); ch.onmessage = upMsg;
  try {
    await invoke("update_stack", { onProgress: ch, config: readConfig() });
    upCard("upDone");
  } catch (e) {
    const m = String(e);
    $("upFailMsg").textContent = /cancel/i.test(m) ? "Update cancelled." : "Update stopped: " + m;
    upCard("upFailed");
  }
}
async function cancelUpdate() {
  const ok = confirm(
    "Cancel the update?\n\n" +
    "The current step stops. Your data is safe, but a partly-applied update may need a cleanup + retry. Docker stays installed."
  );
  if (!ok) return;
  $("upCancelBtn").disabled = true;
  $("upStep").textContent = "Cancelling…";
  try { await invoke("cancel_step"); } catch { /* ignore */ }
}
async function cleanupUpdate() {
  const ok = confirm(
    "Clean up leftovers?\n\n" +
    "Stops and removes the SENTIENT containers and reclaims disk from partial pulls so you can retry cleanly. Docker stays installed."
  );
  if (!ok) return;
  upCard("upProgress");
  $("upLog").textContent = "";
  $("upStep").textContent = "Cleaning up…";
  $("upCancelBtn").disabled = true;
  const ch = new Channel(); ch.onmessage = upMsg;
  try { await invoke("cleanup_install", { onProgress: ch, config: readConfig() }); $("upFailMsg").textContent = "Cleanup done — ready to retry."; }
  catch (e) { $("upFailMsg").textContent = "Cleanup error: " + e; }
  upCard("upFailed");
}

// ---- wiring ------------------------------------------------------------------
$("stRefreshBtn").addEventListener("click", loadStatus);
$("mkShortcutBtn").addEventListener("click", makeShortcut);
$("uninstallBtn").addEventListener("click", uninstall);
$("stStartBtn").addEventListener("click", () => stackAction("start"));
$("stStopBtn").addEventListener("click", () => stackAction("stop"));
$("stRestartBtn").addEventListener("click", () => stackAction("restart"));
$("stOpenBtn").addEventListener("click", () => invoke("open_sentient", { port: readConfig().http_port }));
$("logsRefreshBtn").addEventListener("click", loadLogs);
$("updateBtn").addEventListener("click", runUpdate);
$("upRetryBtn").addEventListener("click", runUpdate);
$("upCancelBtn").addEventListener("click", cancelUpdate);
$("upCleanupBtn").addEventListener("click", cleanupUpdate);
$("upOpenBtn").addEventListener("click", () => invoke("open_sentient", { port: readConfig().http_port }));
