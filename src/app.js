// Frontend for the SENTIENT Backup & Restore desktop app. Uses Tauri's global
// `invoke` / `Channel` (withGlobalTauri = true) — no bundler.

const invoke = window.__TAURI__?.core?.invoke;
const Channel = window.__TAURI__?.core?.Channel;

const $ = (id) => document.getElementById(id);
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
        const lg = el("Log");
        lg.textContent += p.line + "\n";
        lg.scrollTop = lg.scrollHeight;
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

async function connect() {
  if (!invoke) { setStatus("Not running inside Tauri.", true); return; }
  $("connect").disabled = true;
  setStatus("Connecting…");
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
    try {
      await invoke("setting_set", {
        key: "last_conn",
        value: JSON.stringify({ host: c.host, port: c.port, dbname: c.dbname, user: c.user }),
      });
    } catch { /* store unavailable */ }
    renderCategories();
    updateGating();
    showView("backup");
  } catch (e) {
    connected = false;
    setStatus("Error: " + e, true);
    setConnStatus("Connection failed", "err");
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
  } catch { /* no saved settings */ }
}

$("themeToggle").addEventListener("click", toggleTheme);
$("connect").addEventListener("click", connect);
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
// Setup wizard: System check -> Components -> WSL2 -> Docker -> Deploy.
// Ported from the standalone installer; shares invoke/Channel/$ above.
// ===========================================================================
const STEPS = ["checks", "components", "wsl", "docker", "deploy"];
const CHECK_ICON = { pass: "i-pass", setup: "i-todo", fail: "i-fail", unknown: "i-unknown" };

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

// ---- Step 3: WSL2 ------------------------------------------------------------
function wslMode(mode) {
  // mode: "start" | "reboot" | "ready"
  $("wslStart").style.display = mode === "start" ? "" : "none";
  $("wslReboot").style.display = mode === "reboot" ? "" : "none";
  $("wslReady").style.display = mode === "ready" ? "" : "none";
  $("toDocker").disabled = mode !== "ready";
}

async function setupWsl() {
  $("wslBtn").disabled = true;
  $("wslProgress").style.display = "";
  $("wslPbar").classList.add("run");
  $("wslLog").textContent = "";
  $("wslStep").textContent = "Starting…";

  const ch = new Channel();
  ch.onmessage = (p) => {
    if (p.type === "step") $("wslStep").textContent = p.name;
    else if (p.type === "log") {
      const l = $("wslLog");
      l.textContent += p.line + "\n";
      l.scrollTop = l.scrollHeight;
    } else if (p.type === "done") $("wslStep").textContent = "✓ " + p.message;
    else if (p.type === "error") $("wslStep").textContent = "✗ " + p.message;
  };

  try {
    const res = await invoke("install_wsl", { onProgress: ch });
    $("wslPbar").classList.remove("run");
    if (res.ready) {
      await invoke("set_state", { step: "wsl_ready" });
      wslMode("ready");
    } else if (res.reboot_required) {
      await invoke("set_state", { step: "wsl_pending_reboot" });
      await invoke("arm_resume");
      wslMode("reboot");
    }
  } catch (e) {
    $("wslPbar").classList.remove("run");
    $("wslStep").textContent = "Failed: " + e;
    $("wslBtn").disabled = false;
  }
}

// ---- Step 4: Docker ---------------------------------------------------------
function dockerMode(mode) {
  // mode: "start" | "ready"
  $("dockerStart").style.display = mode === "start" ? "" : "none";
  $("dockerReady").style.display = mode === "ready" ? "" : "none";
  $("toDeploy").disabled = mode !== "ready";
}

async function setupDocker() {
  $("dockerBtn").disabled = true;
  $("dockerStart").style.display = "none";
  $("dockerProgress").style.display = "";
  $("dkLog").textContent = "";
  $("dkStep").textContent = "Starting…";
  const bar = $("dkPbar"), fill = bar.querySelector(".fill");
  bar.classList.add("run");
  fill.style.width = "30%";

  const ch = new Channel();
  ch.onmessage = (p) => {
    if (p.type === "step") {
      $("dkStep").textContent = p.name;
      bar.classList.add("run");
      fill.style.width = "30%";
    } else if (p.type === "percent") {
      bar.classList.remove("run");
      fill.style.width = Math.round(p.value * 100) + "%";
    } else if (p.type === "log") {
      const l = $("dkLog");
      l.textContent += p.line + "\n";
      l.scrollTop = l.scrollHeight;
    } else if (p.type === "done") {
      bar.classList.remove("run");
      fill.style.width = "100%";
      $("dkStep").textContent = "✓ " + p.message;
    } else if (p.type === "error") {
      bar.classList.remove("run");
      $("dkStep").textContent = "✗ " + p.message;
    }
  };

  try {
    await invoke("setup_docker", { onProgress: ch });
    await invoke("set_state", { step: "docker_ready" });
    dockerMode("ready");
  } catch (e) {
    bar.classList.remove("run");
    $("dkStep").textContent = "Failed: " + e;
    $("dockerBtn").disabled = false;
    $("dockerStart").style.display = "";
  }
}

// ---- Step 5: Deploy ---------------------------------------------------------
function deployMode(mode) {
  $("deployStart").style.display = mode === "start" ? "" : "none";
  $("deployReady").style.display = mode === "ready" ? "" : "none";
}

async function setupDeploy() {
  $("deployBtn").disabled = true;
  $("deployStart").style.display = "none";
  $("deployProgress").style.display = "";
  $("dpLog").textContent = "";
  $("dpStep").textContent = "Starting…";
  const bar = $("dpPbar"), fill = bar.querySelector(".fill");
  bar.classList.add("run");
  fill.style.width = "30%";

  const ch = new Channel();
  ch.onmessage = (p) => {
    if (p.type === "step") {
      $("dpStep").textContent = p.name;
      bar.classList.add("run");
      fill.style.width = "30%";
    } else if (p.type === "log") {
      const l = $("dpLog");
      l.textContent += p.line + "\n";
      l.scrollTop = l.scrollHeight;
    } else if (p.type === "done") {
      bar.classList.remove("run");
      fill.style.width = "100%";
      $("dpStep").textContent = "✓ " + p.message;
    } else if (p.type === "error") {
      bar.classList.remove("run");
      $("dpStep").textContent = "✗ " + p.message;
    }
  };

  try {
    await invoke("deploy_sentient", { onProgress: ch });
    await invoke("set_state", { step: "deployed" });
    deployMode("ready");
  } catch (e) {
    bar.classList.remove("run");
    $("dpStep").textContent = "Failed: " + e;
    $("deployBtn").disabled = false;
    $("deployStart").style.display = "";
  }
}

// ---- Setup init / resume -----------------------------------------------------
async function initSetup() {
  // Restore the remembered Client choice (M1).
  try {
    const c = await invoke("setting_get", { key: "install_client" });
    if (c === "1") $("compClient").checked = true;
  } catch { /* store unavailable */ }

  if (!invoke) return;
  let state = "checks";
  try { state = await invoke("get_state"); } catch { /* default */ }

  if (state === "deployed") {
    showStep("deploy");
    deployMode("ready");
  } else if (state === "docker_ready") {
    showStep("docker");
    dockerMode("ready");
  } else if (state === "wsl_ready") {
    showStep("wsl");
    wslMode("ready");
  } else if (state === "wsl_pending_reboot") {
    showStep("wsl");
    // resumed after a reboot — re-verify
    const ready = await invoke("wsl_ready").catch(() => false);
    if (ready) {
      await invoke("set_state", { step: "wsl_ready" });
      wslMode("ready");
    } else {
      wslMode("start");
    }
  } else {
    showStep("checks");
    recheck();
  }
}

// ---- Setup wiring ------------------------------------------------------------
$("recheckBtn").addEventListener("click", recheck);
$("toComponents").addEventListener("click", () => showStep("components"));
$("backChecks").addEventListener("click", () => showStep("checks"));
$("toWsl").addEventListener("click", () => {
  try { invoke("setting_set", { key: "install_client", value: $("compClient").checked ? "1" : "0" }); } catch { /* store off */ }
  showStep("wsl");
  wslMode($("wslReady").style.display === "" ? "ready" : "start");
});
$("backComponents").addEventListener("click", () => showStep("components"));
$("wslBtn").addEventListener("click", setupWsl);
$("rebootBtn").addEventListener("click", () => invoke("reboot_now"));
$("rebootLater").addEventListener("click", () => { $("wslReboot").style.display = "none"; });
$("toDocker").addEventListener("click", () => { showStep("docker"); dockerMode($("dockerReady").style.display === "" ? "ready" : "start"); });
$("backWsl").addEventListener("click", () => showStep("wsl"));
$("dockerBtn").addEventListener("click", setupDocker);
$("toDeploy").addEventListener("click", () => { showStep("deploy"); deployMode($("deployReady").style.display === "" ? "ready" : "start"); });
$("backDocker").addEventListener("click", () => showStep("docker"));
$("deployBtn").addEventListener("click", setupDeploy);
$("openBtn").addEventListener("click", () => invoke("open_sentient"));
initSetup();
