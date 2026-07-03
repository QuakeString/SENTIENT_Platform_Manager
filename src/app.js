// SENTIENT Installer frontend. Wizard: System check -> WSL2 -> Docker -> Deploy.
// Uses Tauri's global invoke/Channel (withGlobalTauri), no bundler.

const invoke = window.__TAURI__?.core?.invoke;
const Channel = window.__TAURI__?.core?.Channel;
const $ = (id) => document.getElementById(id);

const STEPS = ["checks", "wsl", "docker", "deploy"];
const ICON = { pass: "i-pass", setup: "i-setup", fail: "i-fail", unknown: "i-unknown" };

function showStep(name) {
  document.querySelectorAll(".chip").forEach((c) => {
    const i = STEPS.indexOf(c.dataset.step), ci = STEPS.indexOf(name);
    c.classList.toggle("active", c.dataset.step === name);
    c.classList.toggle("done", i >= 0 && ci >= 0 && i < ci);
  });
  document.querySelectorAll(".view").forEach((v) => (v.style.display = "none"));
  const view = $(name + "View");
  if (view) view.style.display = "block";
}

// ---- Step 1: checks ----------------------------------------------------------
function renderChecks(list) {
  $("checks").innerHTML = list
    .map(
      (c) => `
      <div class="check">
        <svg class="icon s-${c.status}"><use href="#${ICON[c.status] || "i-unknown"}"/></svg>
        <div>
          <div class="label">${c.label}</div>
          <div class="detail">${c.detail}</div>
        </div>
      </div>`
    )
    .join("");
  const fails = list.filter((c) => c.status === "fail");
  const setups = list.filter((c) => c.status === "setup");
  const s = $("summary");
  if (fails.length) {
    s.className = "summary bad";
    s.textContent = `${fails.length} blocker${fails.length > 1 ? "s" : ""} must be resolved before installing.`;
    $("toWsl").disabled = true;
  } else {
    s.className = "summary";
    s.textContent = setups.length
      ? `Ready — the installer will set up ${setups.length} item${setups.length > 1 ? "s" : ""}.`
      : "Everything's ready.";
    $("toWsl").disabled = false;
  }
}

async function recheck() {
  if (!invoke) return;
  $("recheck").disabled = true;
  $("checks").innerHTML =
    '<div class="check"><svg class="icon spin s-unknown"><use href="#i-spin"/></svg><div><div class="label">Checking…</div></div></div>';
  try {
    renderChecks(await invoke("preflight"));
  } catch (e) {
    $("summary").className = "summary bad";
    $("summary").textContent = "Check failed: " + e;
  } finally {
    $("recheck").disabled = false;
  }
}

// ---- Step 2: WSL2 ------------------------------------------------------------
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

// ---- Step 3: Docker ---------------------------------------------------------
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

// ---- Step 4: Deploy ---------------------------------------------------------
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

// ---- Init / resume ----------------------------------------------------------
async function init() {
  if (!invoke) return;
  let state = "checks";
  try {
    state = await invoke("get_state");
  } catch { /* default */ }

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
      $("wslResumeNote").textContent =
        "Resuming after restart. WSL still needs finishing — click Set up WSL2 to continue.";
    }
  } else {
    showStep("checks");
    recheck();
  }
}

// ---- wiring ------------------------------------------------------------------
$("recheck").addEventListener("click", recheck);
$("toWsl").addEventListener("click", () => { showStep("wsl"); wslMode($("wslReady").style.display === "" ? "ready" : "start"); });
$("backChecks").addEventListener("click", () => showStep("checks"));
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
init();
