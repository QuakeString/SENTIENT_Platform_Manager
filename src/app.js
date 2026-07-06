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
  initTheme();
  updateGating();
  if (!invoke) return;
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
const CFG_DEFAULTS = {
  db_name: "sentient", db_user: "sentient", db_password: "sentient",
  http_port: 8080, mqtt_port: 1883, coap_port: 5683, load_demo: false,
};
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
  if (fails.length) {
    s.className = "summary bad";
    s.textContent = `${fails.length} blocker${fails.length > 1 ? "s" : ""} must be resolved before installing.`;
    $("toComponents").disabled = true;
  } else {
    s.className = "summary";
    s.textContent = setups.length
      ? `Ready — the installer will set up ${setups.length} item${setups.length > 1 ? "s" : ""}.`
      : "Everything's ready.";
    $("toComponents").disabled = false;
  }
}

async function recheck() {
  if (!invoke) return;
  $("recheckBtn").disabled = true;
  $("checksList").innerHTML =
    '<div class="check"><svg class="icon spin s-unknown"><use href="#i-spin"/></svg><div><div class="label">Checking…</div></div></div>';
  try {
    renderChecks(await invoke("preflight"));
  } catch (e) {
    $("checksSummary").className = "summary bad";
    $("checksSummary").textContent = "Check failed: " + e;
  } finally {
    $("recheckBtn").disabled = false;
  }
}

// ---- Step 3: configure -------------------------------------------------------
function readConfig() {
  const num = (id, d) => { const v = parseInt($(id).value, 10); return Number.isFinite(v) ? v : d; };
  return {
    db_name: ($("cfgDbName").value || "").trim() || CFG_DEFAULTS.db_name,
    db_user: ($("cfgDbUser").value || "").trim() || CFG_DEFAULTS.db_user,
    db_password: $("cfgDbPass").value || CFG_DEFAULTS.db_password,
    http_port: num("cfgHttpPort", CFG_DEFAULTS.http_port),
    mqtt_port: num("cfgMqttPort", CFG_DEFAULTS.mqtt_port),
    coap_port: num("cfgCoapPort", CFG_DEFAULTS.coap_port),
    load_demo: $("cfgLoadDemo").checked,
  };
}
function applyConfig(c) {
  $("cfgDbName").value = c.db_name; $("cfgDbUser").value = c.db_user; $("cfgDbPass").value = c.db_password;
  $("cfgHttpPort").value = c.http_port; $("cfgMqttPort").value = c.mqtt_port; $("cfgCoapPort").value = c.coap_port;
  $("cfgLoadDemo").checked = !!c.load_demo;
}
function renderReco() {
  const c = readConfig();
  const rows = [
    ["Database", `${c.db_name} (user ${c.db_user})`],
    ["Web address", `http://localhost:${c.http_port}`],
    ["MQTT / CoAP ports", `${c.mqtt_port} / ${c.coap_port}`],
    ["Demo data", c.load_demo ? "Yes" : "No (clean install)"],
    ["Kiosk launcher", $("compKiosk").checked ? "Desktop shortcut (Chrome if needed)" : "Skip"],
  ];
  $("recoSummary").innerHTML = rows
    .map(([k, v]) => `<tr><td class="cat-note" style="width:42%">${k}</td><td>${v}</td></tr>`)
    .join("");
}
function validateConfig(c) {
  const ports = [["Web", c.http_port], ["MQTT", c.mqtt_port], ["CoAP", c.coap_port]];
  for (const [n, p] of ports) if (!(p >= 1 && p <= 65535)) return `${n} port must be between 1 and 65535.`;
  if (new Set(ports.map(([, p]) => p)).size !== ports.length) return "The Web, MQTT and CoAP ports must all be different.";
  if (!c.db_name || !c.db_user) return "Database name and user can't be empty.";
  return null;
}
async function persistConfig() {
  try { await invoke("setting_set", { key: "install_config", value: JSON.stringify(readConfig()) }); } catch { /* store off */ }
}
async function loadConfig() {
  try {
    const s = await invoke("setting_get", { key: "install_config" });
    if (s) applyConfig({ ...CFG_DEFAULTS, ...JSON.parse(s) });
  } catch { /* no saved config */ }
  renderReco();
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

async function autoInstall() {
  if (installing || !invoke) return;
  installing = true;
  installCard("installProgress");
  $("cancelInstallBtn").disabled = false;
  const kiosk = $("compKiosk").checked;
  const total = kiosk ? 4 : 3;
  try {
    // Phase 1 — WSL2 (may require a reboot; resumes automatically after)
    setPhase(1, total, "WSL2");
    if (!(await invoke("wsl_ready"))) {
      const res = await invoke("install_wsl", { onProgress: instChannel() });
      if (res.reboot_required) {
        await invoke("set_state", { step: "wsl_pending_reboot" });
        await invoke("arm_resume");
        installing = false;
        installCard("installReboot");
        return;
      }
    }
    await invoke("set_state", { step: "wsl_ready" });

    // Phase 2 — Docker Engine
    setPhase(2, total, "Docker Engine");
    if (!(await invoke("docker_ready"))) {
      await invoke("setup_docker", { onProgress: instChannel() });
    }
    await invoke("set_state", { step: "docker_ready" });

    // Phase 3 — deploy the stack with the chosen config
    setPhase(3, total, "Deploy SENTIENT");
    await invoke("deploy_sentient", { onProgress: instChannel(), config: readConfig() });
    await invoke("set_state", { step: "deployed" });

    // Point the Backup tab at the freshly-installed local DB and connect it, so
    // "Connection" is ready without the user re-entering anything.
    try {
      const c = readConfig();
      $("host").value = "localhost";
      $("port").value = 5432;
      $("dbname").value = c.db_name;
      $("user").value = c.db_user;
      $("password").value = c.db_password;
      await invoke("setting_set", {
        key: "last_conn",
        value: JSON.stringify({ host: "localhost", port: 5432, dbname: c.db_name, user: c.db_user }),
      });
      await invoke("set_last_password", { password: c.db_password });
      connect(false, true); // quiet, no view switch
    } catch { /* non-fatal */ }

    // Phase 4 — optional desktop kiosk launcher (installs Chrome if needed).
    // Non-fatal: SENTIENT is already up, so a shortcut hiccup shouldn't fail it.
    if (kiosk) {
      setPhase(4, total, "Desktop kiosk shortcut");
      try {
        await invoke("create_kiosk_shortcut", { onProgress: instChannel(), port: readConfig().http_port });
      } catch (e) {
        instMsg({ type: "log", line: "Shortcut step skipped: " + e });
      }
    }

    installing = false;
    showInstallDone();
  } catch (e) {
    installing = false;
    const msg = String(e);
    $("failMsg").textContent = /cancel/i.test(msg) ? "Install cancelled." : "Install stopped: " + msg;
    installCard("installFailed");
  }
}

function showInstallDone() {
  const url = `http://localhost:${readConfig().http_port}`;
  $("doneHint").innerHTML =
    `Open <b>${url}</b> and sign in as sys-admin:<br>&nbsp;&nbsp;<b>admin@sentient.local</b> &nbsp;/&nbsp; <b>admin123</b><br>It starts automatically on login from now on.`;
  installCard("installDone");
}

async function cancelInstall() {
  const ok = confirm(
    "Cancel the install?\n\n" +
    "The step that's running now will be stopped. This can leave half-downloaded images or partially-created containers behind — you'll be offered a cleanup afterwards. Anything that already finished (WSL2, Docker) stays installed."
  );
  if (!ok) return;
  $("cancelInstallBtn").disabled = true;
  $("instStep").textContent = "Cancelling…";
  try { await invoke("cancel_step"); } catch { /* ignore */ }
}

async function cleanupLeftovers() {
  const ok = confirm(
    "Clean up leftovers?\n\n" +
    "This stops and removes the SENTIENT containers and their data volumes, and reclaims disk from partially-pulled images, so you can retry from a clean state. WSL2 and Docker Engine stay installed."
  );
  if (!ok) return;
  installCard("installProgress");
  setPhase(1, 1, "Cleanup");
  $("cancelInstallBtn").disabled = true;
  try {
    await invoke("cleanup_install", { onProgress: instChannel() });
    $("failMsg").textContent = "Cleanup done — ready to retry.";
  } catch (e) {
    $("failMsg").textContent = "Cleanup error: " + e;
  }
  installCard("installFailed");
}

// ---- Setup init / resume -----------------------------------------------------
async function initSetup() {
  try { const c = await invoke("setting_get", { key: "install_kiosk" }); if (c === "0") $("compKiosk").checked = false; } catch { /* store off */ }
  await loadConfig();
  $("installPlan").innerHTML =
    "Ready to set up <b>WSL2</b>, <b>Docker Engine</b>, and deploy the <b>SENTIENT</b> stack. This can take several minutes and may restart your PC once.";

  if (!invoke) return;
  let state = "checks";
  try { state = await invoke("get_state"); } catch { /* default */ }

  if (state === "deployed") {
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
}

// ---- Setup wiring ------------------------------------------------------------
$("recheckBtn").addEventListener("click", recheck);
$("toComponents").addEventListener("click", () => showStep("components"));
$("toConfigure").addEventListener("click", () => { renderReco(); showStep("configure"); });
$("backChecks").addEventListener("click", () => showStep("checks"));
$("customizeBtn").addEventListener("click", () => { $("recoCard").style.display = "none"; $("customCard").style.display = ""; });
$("resetDefaultsBtn").addEventListener("click", () => { applyConfig(CFG_DEFAULTS); $("cfgError").textContent = ""; });
$("backComponents").addEventListener("click", () => showStep("components"));
$("toInstall").addEventListener("click", async () => {
  const c = readConfig();
  const err = validateConfig(c);
  if (err) { $("cfgError").textContent = err; $("recoCard").style.display = "none"; $("customCard").style.display = ""; return; }
  $("cfgError").textContent = "";
  try { await invoke("setting_set", { key: "install_kiosk", value: $("compKiosk").checked ? "1" : "0" }); } catch { /* store off */ }
  await persistConfig();
  showStep("install");
  installCard("installStart");
});
$("startInstallBtn").addEventListener("click", autoInstall);
$("retryInstallBtn").addEventListener("click", autoInstall);
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
  try { st = await invoke("stack_status", { port: readConfig().http_port }); } catch { return; }
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
  $("containers").innerHTML = st.containers.length
    ? st.containers.map((c) => `<tr><td>${c.name}</td><td>${fmtState(c.state)}</td><td class="cat-note">${c.status}</td></tr>`).join("")
    : `<tr><td colspan="3" class="cat-note">No containers yet.</td></tr>`;
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
  try { await invoke("stack_control", { action, onProgress: ch }); }
  catch (e) { $("stStep").textContent = "Failed: " + e; }
  finally { await loadStatus(); }
}

async function loadLogs() {
  $("logsBox").textContent = "Loading…";
  try { $("logsBox").textContent = (await invoke("stack_logs", { tail: 300 })) || "(no output)"; }
  catch (e) { $("logsBox").textContent = "Error: " + e; }
}

async function uninstall() {
  const ok = confirm(
    "Uninstall SENTIENT?\n\n" +
    "This permanently deletes the SENTIENT containers, the database and ALL its data (volumes), and the WSL distro, and removes the desktop shortcut and autostart. WSL2 and this Manager app are kept. This cannot be undone."
  );
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
    await invoke("uninstall_sentient", { onProgress: ch });
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
  try { st = await invoke("stack_status", { port: readConfig().http_port }); } catch { return; }
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
    await invoke("update_stack", { onProgress: ch });
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
    "The current step stops. Your data is safe, but a partly-applied update may need a cleanup + retry. WSL2 and Docker stay installed."
  );
  if (!ok) return;
  $("upCancelBtn").disabled = true;
  $("upStep").textContent = "Cancelling…";
  try { await invoke("cancel_step"); } catch { /* ignore */ }
}
async function cleanupUpdate() {
  const ok = confirm(
    "Clean up leftovers?\n\n" +
    "Stops and removes the SENTIENT containers and reclaims disk from partial pulls so you can retry cleanly. WSL2 and Docker stay installed."
  );
  if (!ok) return;
  upCard("upProgress");
  $("upLog").textContent = "";
  $("upStep").textContent = "Cleaning up…";
  $("upCancelBtn").disabled = true;
  const ch = new Channel(); ch.onmessage = upMsg;
  try { await invoke("cleanup_install", { onProgress: ch }); $("upFailMsg").textContent = "Cleanup done — ready to retry."; }
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
