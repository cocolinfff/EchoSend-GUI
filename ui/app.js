const { invoke } = window.__TAURI__.core;

const STATUS_MAP = {
  SEEDING: { label: "Seeding", cls: "done" },
  COMPLETE: { label: "Complete", cls: "done" },
  DOWNLOADING: { label: "Downloading", cls: "dl" },
  KNOWN: { label: "Known", cls: "known" },
  FAILED: { label: "Failed", cls: "fail" },
};

const state = {
  daemonRunning: false,
  autostartEnabled: false,
  contextMenuEnabled: false,
  refreshSeconds: 3,
  fileMap: new Map(),
  downloadStats: new Map(),
  daemonLogs: [],
  logFilterHideSync: false,
  updateInfo: null,
  updateChecking: false,
  contextFile: null,
};

const $ = (id) => document.getElementById(id);

function fmtTime(ns) {
  if (!ns || Number.isNaN(ns)) {
    return "";
  }
  const d = new Date(Number(ns) / 1e6);
  const pad = (x) => String(x).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtSize(n) {
  const size = Number(n || 0);
  if (size < 1024) return `${size} B`;
  if (size < 1024 ** 2) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 ** 3) return `${(size / 1024 ** 2).toFixed(1)} MB`;
  return `${(size / 1024 ** 3).toFixed(1)} GB`;
}

function setDaemonPill(running) {
  const pill = $("daemonStatus");
  pill.textContent = running ? "daemon running" : "daemon stopped";
  pill.classList.toggle("ok", running);
  pill.classList.toggle("warning", !running);
  $("btnDaemon").textContent = running ? "Stop Daemon" : "Start Daemon";
  state.daemonRunning = running;
}

function showError(err) {
  const msg = typeof err === "string" ? err : JSON.stringify(err);
  window.alert(msg);
}

function setUpdateChecking(checking) {
  state.updateChecking = checking;
  const btn = $("btnUpdateKernel");
  btn.disabled = checking;
  btn.textContent = checking ? "Checking..." : "Check / Update";
}

async function loadSnapshot() {
  const snap = await invoke("get_snapshot");
  if (snap.kernel.exists) {
    const ver = snap.kernel_version || "unknown";
    $("kernelStatus").textContent = `Kernel: ${snap.kernel.path} (v${ver})`;
  } else {
    $("kernelStatus").textContent = "Kernel: not found";
  }
  setDaemonPill(snap.daemon_running);
  $("peerStatus").textContent = `peers: ${snap.peers ?? "-"}`;

  const cfg = snap.startup_config;
  state.refreshSeconds = Math.max(1, Number(cfg.refresh_seconds || 3));
  $("refreshSeconds").value = String(state.refreshSeconds);
  $("daemonArgs").value = (cfg.daemon_args || []).join("\n");
  $("autoStartDaemon").checked = Boolean(cfg.auto_start_daemon);
  $("launchMinimized").checked = Boolean(cfg.launch_minimized_to_tray);
  $("closeToTray").checked = cfg.close_to_tray !== false;

  state.autostartEnabled = !!snap.autostart_enabled;
  state.contextMenuEnabled = !!snap.context_menu_enabled;
  renderToggles();
}

function renderToggles() {
  $("btnToggleAutostart").textContent = state.autostartEnabled ? "Disable" : "Enable";
  $("btnToggleContext").textContent = state.contextMenuEnabled ? "Disable" : "Enable";
}

async function checkUpdateInfo() {
  setUpdateChecking(true);
  try {
    const info = await invoke("check_kernel_update");
    state.updateInfo = info;

    const cur = info.current_version || "not installed";
    const latest = info.latest_version || "unknown";
    $("updateStatus").textContent = info.has_update
      ? `Update: ${cur} -> ${latest}`
      : `Update: up-to-date (${cur})`;
  } catch (_err) {
    $("updateStatus").textContent = "Update: check failed";
  } finally {
    setUpdateChecking(false);
  }
}

function normalizeHistory(data) {
  const msgItems = (data.messages || []).map((m) => ({ ...m, _type: "msg" }));
  const fileItems = (data.files || []).map((f) => ({ ...f, _type: "file" }));
  return [...msgItems, ...fileItems].sort((a, b) => Number(a.timestamp || 0) - Number(b.timestamp || 0));
}

function rowAction(item) {
  if (item._type === "msg") return "-";
  const st = item.status || "";
  if (["SEEDING", "COMPLETE"].includes(st)) return "Open";
  if (["KNOWN", "FAILED"].includes(st)) return "Pull";
  return "Reveal";
}

function statusBadge(status) {
  const cfg = STATUS_MAP[status] || { label: status || "Unknown", cls: "known" };
  return `<span class="badge ${cfg.cls}">${cfg.label}</span>`;
}

function normalizeName(name) {
  return String(name || "").trim().toLowerCase();
}

function parseDownloadStats(logs) {
  const stats = new Map();

  for (const lineRaw of logs || []) {
    const line = String(lineRaw || "").trim();
    if (!line) continue;

    let m = line.match(/^\[sync\]\s+downloading\s+(.+?)\s+from\s+/i);
    if (m) {
      const key = normalizeName(m[1]);
      stats.set(key, {
        state: "DOWNLOADING",
        detail: "starting...",
      });
      continue;
    }

    m = line.match(/^\[tcp\]\s+(.+?)\s+progress\s+([0-9.]+)%\s+\(([^)]+)\)\s+(.+)$/i);
    if (m) {
      const key = normalizeName(m[1]);
      stats.set(key, {
        state: "DOWNLOADING",
        percent: m[2],
        transferred: m[3],
        speed: m[4],
        detail: `${m[2]}% | ${m[4]}`,
      });
      continue;
    }

    m = line.match(/^\[tcp\]\s+downloaded\s+(.+?)\s+\(/i);
    if (m) {
      const key = normalizeName(m[1]);
      stats.set(key, {
        state: "COMPLETE",
        detail: "done",
      });
      continue;
    }

    m = line.match(/^\[sync\].*?(?:giving up on|failed for)\s+(.+)$/i);
    if (m) {
      const key = normalizeName(m[1]);
      stats.set(key, {
        state: "FAILED",
        detail: "failed",
      });
    }
  }

  return stats;
}

function hasSyncTag(line) {
  const text = String(line || "").replace(/\x1b\[[0-9;]*m/g, "");
  return /\[sync\]/i.test(text);
}

function renderDaemonLog() {
  const logs = state.logFilterHideSync
    ? state.daemonLogs.filter((line) => !hasSyncTag(line))
    : state.daemonLogs;
  $("daemonLog").textContent = logs.join("\n");
}

function hideFileContextMenu() {
  const menu = $("fileContextMenu");
  if (!menu) return;
  menu.classList.remove("show");
}

function showFileContextMenu(item, x, y) {
  const menu = $("fileContextMenu");
  if (!menu) return;

  state.contextFile = item;

  const cancelBtn = $("ctxCancelDownload");
  const status = String(item.status || "").toUpperCase();
  cancelBtn.disabled = status !== "DOWNLOADING";

  const maxX = window.innerWidth - 190;
  const maxY = window.innerHeight - 120;
  menu.style.left = `${Math.max(8, Math.min(x, maxX))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, maxY))}px`;
  menu.classList.add("show");
}

function renderHistory(data) {
  const rowsEl = $("historyRows");
  rowsEl.innerHTML = "";
  state.fileMap.clear();

  const items = normalizeHistory(data);
  for (const item of items) {
    const tr = document.createElement("tr");
    const sender = item.sender_name || item.sender_ip || "-";
    let content = item.content || "";
    let size = "";
    let statusHtml = "<span class=\"badge known\">Message</span>";

    if (item._type === "file") {
      content = item.file_name || "-";
      size = fmtSize(item.file_size);
      statusHtml = statusBadge(item.status || "");

      const stat = state.downloadStats.get(normalizeName(item.file_name || ""));
      if (stat && stat.detail) {
        statusHtml += `<div class="sub-status">${stat.detail}</div>`;
      }
    }

    const action = rowAction(item);

    tr.innerHTML = `
      <td>${fmtTime(item.timestamp)}</td>
      <td>${sender}</td>
      <td>${content}</td>
      <td>${size}</td>
      <td>${statusHtml}</td>
      <td><button class="btn mini ghost">${action}</button></td>
    `;

    tr.querySelector("button").addEventListener("click", () => onRowAction(item));
    if (item._type === "file") {
      tr.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        showFileContextMenu(item, e.clientX, e.clientY);
      });
    }
    rowsEl.appendChild(tr);

    if (item._type === "file" && item.file_hash) {
      state.fileMap.set(item.file_hash, item);
    }
  }

  rowsEl.scrollTop = rowsEl.scrollHeight;
}

async function onRowAction(item) {
  if (item._type !== "file") return;

  const path = item.local_path || "";
  const status = item.status || "";

  if (["SEEDING", "COMPLETE"].includes(status) && path) {
    try {
      await invoke("open_path", { path });
    } catch (err) {
      showError(err);
    }
    return;
  }

  if (["KNOWN", "FAILED"].includes(status)) {
    try {
      await invoke("pull_file", { fileHash: item.file_hash || "" });
    } catch (err) {
      showError(err);
    }
    return;
  }

  if (path) {
    try {
      await invoke("reveal_in_folder", { path });
    } catch (err) {
      showError(err);
    }
  }
}

async function refreshData() {
  try {
    const [history, peers, logs] = await Promise.all([
      invoke("get_history"),
      invoke("get_peers"),
      invoke("get_daemon_logs", { limit: 200 }),
    ]);
    state.downloadStats = parseDownloadStats(logs || []);
    state.daemonLogs = logs || [];
    renderHistory(history);
    $("peerStatus").textContent = `peers: ${peers ?? "-"}`;
    renderDaemonLog();
  } catch (err) {
    console.error(err);
  }
}

function bindTabs() {
  const tabs = ["send", "settings", "log"];
  for (const name of tabs) {
    const tab = $(`tab-${name}`);
    const view = $(`view-${name}`);
    tab.addEventListener("click", () => {
      for (const n of tabs) {
        $(`tab-${n}`).classList.remove("active");
        $(`view-${n}`).classList.remove("active");
      }
      tab.classList.add("active");
      view.classList.add("active");
    });
  }
}

function collectPathsFromFileInput() {
  const files = $("fileInput").files || [];
  const paths = [];
  for (const f of files) {
    if (f.path) paths.push(f.path);
  }
  return paths;
}

function setupDropzone() {
  const zone = $("dropZone");

  zone.addEventListener("dragover", (e) => {
    e.preventDefault();
    zone.classList.add("dragging");
  });

  zone.addEventListener("dragleave", () => zone.classList.remove("dragging"));

  zone.addEventListener("drop", async (e) => {
    e.preventDefault();
    zone.classList.remove("dragging");

    const dt = e.dataTransfer;
    const paths = [];

    if (dt?.files) {
      for (const f of dt.files) {
        if (f.path) paths.push(f.path);
      }
    }

    if (!paths.length) return;

    try {
      await invoke("send_files", { paths });
      await refreshData();
    } catch (err) {
      showError(err);
    }
  });
}

function bindActions() {
  document.addEventListener("click", () => hideFileContextMenu());
  $("fileContextMenu").addEventListener("click", (e) => e.stopPropagation());
  $("ctxCancelDownload").addEventListener("click", async () => {
    hideFileContextMenu();
    const file = state.contextFile;
    if (!file || !file.file_hash) {
      return;
    }
    try {
      const msg = await invoke("cancel_pull", { fileHash: file.file_hash });
      window.alert(msg || "pull cancelled");
      await refreshData();
    } catch (err) {
      showError(err);
    }
  });

  $("btnUpdateKernel").addEventListener("click", async () => {
    try {
      await checkUpdateInfo();
      const info = state.updateInfo;
      if (!info || !info.has_update) {
        window.alert("already up to date or unable to detect latest version");
        return;
      }

      const current = info.current_version || "not installed";
      const latest = info.latest_version || "unknown";
      const ok = window.confirm(`Update kernel from ${current} to ${latest}?`);
      if (!ok) {
        return;
      }

      const msg = await invoke("download_latest_kernel");
      window.alert(msg);
      await loadSnapshot();
      await checkUpdateInfo();
    } catch (err) {
      showError(err);
    }
  });

  $("btnDaemon").addEventListener("click", async () => {
    try {
      if (state.daemonRunning) {
        await invoke("stop_daemon");
        setDaemonPill(false);
      } else {
        await invoke("start_daemon");
        setDaemonPill(true);
      }
      await refreshData();
    } catch (err) {
      showError(err);
    }
  });

  $("btnRefresh").addEventListener("click", refreshData);

  $("logFilterSync").addEventListener("change", (e) => {
    state.logFilterHideSync = Boolean(e.target.checked);
    renderDaemonLog();
  });

  $("btnSendMessage").addEventListener("click", async () => {
    const text = $("messageInput").value.trim();
    if (!text) return;
    try {
      await invoke("send_message", { text });
      $("messageInput").value = "";
      await refreshData();
    } catch (err) {
      showError(err);
    }
  });

  $("messageInput").addEventListener("keydown", async (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      $("btnSendMessage").click();
    }
  });

  $("btnSendFiles").addEventListener("click", async () => {
    const paths = collectPathsFromFileInput();
    if (!paths.length) return;
    try {
      await invoke("send_files", { paths });
      await refreshData();
    } catch (err) {
      showError(err);
    }
  });

  $("btnSaveConfig").addEventListener("click", async () => {
    const daemon_args = $("daemonArgs").value
      .split("\n")
      .map((v) => v.trim())
      .filter(Boolean);

    const refresh_seconds = Math.max(1, Number($("refreshSeconds").value || 3));
    const auto_start_daemon = $("autoStartDaemon").checked;
    const launch_minimized_to_tray = $("launchMinimized").checked;
    const close_to_tray = $("closeToTray").checked;

    try {
      await invoke("save_startup_config", {
        config: {
          daemon_args,
          auto_start_daemon,
          refresh_seconds,
          launch_minimized_to_tray,
          close_to_tray,
        },
      });
      state.refreshSeconds = refresh_seconds;
      window.alert("settings saved");
    } catch (err) {
      showError(err);
    }
  });

  $("btnToggleAutostart").addEventListener("click", async () => {
    try {
      await invoke("set_autostart_enabled", { enable: !state.autostartEnabled });
      state.autostartEnabled = !state.autostartEnabled;
      renderToggles();
    } catch (err) {
      showError(err);
    }
  });

  $("btnToggleContext").addEventListener("click", async () => {
    try {
      await invoke("set_context_menu_enabled", { enable: !state.contextMenuEnabled });
      state.contextMenuEnabled = !state.contextMenuEnabled;
      renderToggles();
    } catch (err) {
      showError(err);
    }
  });

  $("btnHideTray").addEventListener("click", async () => {
    try {
      await invoke("hide_to_tray");
    } catch (err) {
      showError(err);
    }
  });

  $("btnQuit").addEventListener("click", async () => {
    try {
      await invoke("quit_app");
    } catch (err) {
      showError(err);
    }
  });
}

function startLoop() {
  async function loop() {
    await refreshData();
    setTimeout(loop, Math.max(1000, state.refreshSeconds * 1000));
  }
  loop();
}

async function bootstrap() {
  bindTabs();
  setupDropzone();
  bindActions();

  await loadSnapshot();
  await checkUpdateInfo();
  await refreshData();
  startLoop();
}

bootstrap().catch((err) => {
  console.error(err);
  showError(err);
});
