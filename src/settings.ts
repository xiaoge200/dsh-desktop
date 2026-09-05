import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { SETTINGS_STRINGS, detectLang, tr } from "./i18n";

const lang = detectLang();
const dict = SETTINGS_STRINGS[lang];
const T = (k: string) => tr(dict, k);

function applyStaticI18n() {
  document.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    const key = el.getAttribute("data-i18n");
    if (key) el.textContent = T(key);
  });
}
applyStaticI18n();

interface ServiceState {
  port: number;
  service_running: boolean;
  phase: string;
  error: string | null;
  op_busy?: boolean;
}

interface SettingsData extends ServiceState {
  app_version: string;
  node_version: string | null;
  dsh_version: string | null;
  workspace_dir: string;
  log_file: string;
  autostart_enabled: boolean;
}

interface AppConfig {
  auto_update_dsh: boolean;
  auto_update_app: boolean;
  port: number;
  registry_source: string;
}

interface DshUpdateStatus {
  ok: boolean;
  update_available: boolean;
  current: string | null;
  latest: string | null;
  prerelease: string | null;
  pre_available: boolean;
  message: string;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.querySelector(id) as T;

const els = {
  autostart: document.querySelector("#autostart") as HTMLInputElement,
  autoUpdate: document.querySelector("#auto-update") as HTMLInputElement,
  serviceState: $("#service-state"),
  restartBtn: $("#restart-btn") as HTMLButtonElement,
  appVersion: $("#app-version"),
  dshVersion: $("#dsh-version"),
  nodeVersion: $("#node-version"),
  workspacePath: $("#workspace-path"),
  logPath: $("#log-path"),
  openWorkspaceBtn: $("#open-workspace-btn") as HTMLButtonElement,
  openLogBtn: $("#open-log-btn") as HTMLButtonElement,
  portInput: $("#port-input") as HTMLInputElement,
  registrySelect: $("#registry-select") as HTMLSelectElement,
  advancedSaveBtn: $("#advanced-save-btn") as HTMLButtonElement,
  advancedSaveState: $("#advanced-save-state"),
  updateState: $("#dsh-update-state"),
  checkUpdateBtn: $("#check-update-btn") as HTMLButtonElement,
  applyUpdateBtn: $("#apply-update-btn") as HTMLButtonElement,
  appUpdateState: $("#app-update-state"),
  appCheckUpdateBtn: $("#app-check-update-btn") as HTMLButtonElement,
  appApplyUpdateBtn: $("#app-apply-update-btn") as HTMLButtonElement,
};

let configCache: AppConfig | null = null;
let lastService: ServiceState | null = null;
let restartBusy = false;
let transitionSince: number | null = null;

function setRowText(el: HTMLElement, text: string) {
  el.textContent = text;
}

let lastUpdateStatus: DshUpdateStatus | null = null;
let updateBusy = false;

function renderUpdateStatus() {
  const st = lastUpdateStatus;
  if (!st) {
    setRowText(els.updateState, T("尚未检查更新"));
    els.applyUpdateBtn.hidden = true;
    return;
  }

  let msg = st.message;
  if (st.pre_available && st.prerelease && !st.update_available) {

    msg = lang === "zh"
      ? `已是最新正式版；发现预发布版本 ${st.prerelease}（已开启预发布更新）`
      : `Latest stable installed; prerelease ${st.prerelease} available (prerelease updates on)`;
  } else if (st.prerelease && st.pre_available) {
    msg = lang === "zh"
      ? `${st.message}（预发布 ${st.prerelease}）`
      : `${st.message} (prerelease ${st.prerelease})`;
  }
  if (!st.ok && !/占用/.test(msg)) {
    msg = `${msg} ${T("文件占用提示")}`;
  }
  setRowText(els.updateState, msg);

  els.applyUpdateBtn.hidden = !st.update_available || updateBusy;

  const ver = st.current ?? st.latest ?? st.prerelease;
  if (ver) setRowText(els.dshVersion, ver);
}

async function loadUpdateStatus() {
  try {
    lastUpdateStatus = await invoke<DshUpdateStatus | null>("get_dsh_update_status");
  } catch (e) {
    console.error("load update status failed", e);
    lastUpdateStatus = null;
  }
  renderUpdateStatus();
}

async function doCheckUpdate() {
  if (updateBusy) return;
  updateBusy = true;
  els.checkUpdateBtn.disabled = true;
  els.checkUpdateBtn.textContent = T("检查中…");
  setRowText(els.updateState, T("检查中…"));
  try {
    lastUpdateStatus = await invoke<DshUpdateStatus>("check_dsh_update");
  } catch (e) {
    console.error("check update failed", e);
    lastUpdateStatus = {
      ok: false,
      update_available: false,
      current: null,
      latest: null,
      prerelease: null,
      pre_available: false,
      message: lang === "zh" ? "检查更新失败，请查看日志。" : "Failed to check updates. See the logs.",
    };
  } finally {
    updateBusy = false;
    els.checkUpdateBtn.disabled = false;
    els.checkUpdateBtn.textContent = T("检查更新");
    renderUpdateStatus();
  }
}

async function doApplyUpdate() {
  if (updateBusy) return;
  updateBusy = true;
  els.applyUpdateBtn.disabled = true;
  els.applyUpdateBtn.textContent = T("更新中…");
  setRowText(els.updateState, T("正在更新，请稍候…"));
  try {
    lastUpdateStatus = await invoke<DshUpdateStatus>("update_dsh");
  } catch (e) {
    console.error("update failed", e);
    lastUpdateStatus = {
      ok: false,
      update_available: false,
      current: null,
      latest: null,
      prerelease: null,
      pre_available: false,
      message: lang === "zh" ? "更新失败，请查看日志。" : "Update failed. See the logs.",
    };
  } finally {
    updateBusy = false;
    els.applyUpdateBtn.disabled = false;
    els.applyUpdateBtn.textContent = T("立即更新");
    renderUpdateStatus();
  }
}

let lastAppUpdateStatus: DshUpdateStatus | null = null;
let appUpdateBusy = false;

function renderAppUpdateStatus() {
  const st = lastAppUpdateStatus;
  if (!st) {
    setRowText(els.appUpdateState, T("尚未检查更新"));
    els.appApplyUpdateBtn.hidden = true;
    return;
  }
  setRowText(els.appUpdateState, st.message);
  els.appApplyUpdateBtn.hidden = !st.update_available || appUpdateBusy;
  const ver = st.current ?? st.latest;
  if (ver) setRowText(els.appVersion, ver);
}

async function loadAppUpdateStatus() {
  try {
    lastAppUpdateStatus = await invoke<DshUpdateStatus | null>("get_app_update_status");
  } catch (e) {
    console.error("load app update status failed", e);
    lastAppUpdateStatus = null;
  }
  renderAppUpdateStatus();
}

async function doCheckAppUpdate() {
  if (appUpdateBusy) return;
  appUpdateBusy = true;
  els.appCheckUpdateBtn.disabled = true;
  els.appCheckUpdateBtn.textContent = T("检查中…");
  setRowText(els.appUpdateState, T("检查中…"));
  try {
    lastAppUpdateStatus = await invoke<DshUpdateStatus>("check_app_update");
  } catch (e) {
    console.error("check app update failed", e);
    lastAppUpdateStatus = {
      ok: false,
      update_available: false,
      current: null,
      latest: null,
      prerelease: null,
      pre_available: false,
      message: lang === "zh" ? "检查更新失败，请查看日志。" : "Failed to check updates. See the logs.",
    };
  } finally {
    appUpdateBusy = false;
    els.appCheckUpdateBtn.disabled = false;
    els.appCheckUpdateBtn.textContent = T("检查更新");
    renderAppUpdateStatus();
  }
}

async function doApplyAppUpdate() {
  if (appUpdateBusy) return;
  appUpdateBusy = true;
  els.appApplyUpdateBtn.disabled = true;
  els.appApplyUpdateBtn.textContent = T("更新中…");
  setRowText(els.appUpdateState, T("正在更新，请稍候…"));
  try {
    lastAppUpdateStatus = await invoke<DshUpdateStatus>("update_app");
  } catch (e) {
    console.error("app update failed", e);
    lastAppUpdateStatus = {
      ok: false,
      update_available: false,
      current: null,
      latest: null,
      prerelease: null,
      pre_available: false,
      message: lang === "zh" ? "更新失败，请查看日志。" : "Update failed. See the logs.",
    };
  } finally {
    appUpdateBusy = false;
    els.appApplyUpdateBtn.disabled = false;
    els.appApplyUpdateBtn.textContent = T("立即更新");
    renderAppUpdateStatus();
  }
}

function serviceStateText(s: ServiceState): string {
  if (s.service_running) {
    return lang === "zh" ? `运行中（端口 ${s.port}）` : `Running (port ${s.port})`;
  }
  if (s.phase === "node-check" || s.phase === "dsh-install" || s.phase === "service-start") {
    return T("正在启动…");
  }
  if (s.phase === "error") {
    return s.error ?? T("启动失败");
  }
  return s.port > 0 ? T("正在重启…") : T("未启动");
}

function renderRestartButton(s: ServiceState) {
  if (restartBusy) return;
  const booting = s.phase === "node-check" || s.phase === "dsh-install";
  const starting = s.phase === "service-start";
  const transitioning = !s.service_running && s.phase !== "error" && s.port > 0;
  const queued = s.op_busy && s.service_running;
  if (booting) {
    transitionSince = null;
    els.restartBtn.disabled = true;
    els.restartBtn.textContent = T("正在启动…");
  } else if (starting) {
    transitionSince = null;
    els.restartBtn.disabled = true;
    els.restartBtn.textContent = T("正在重启…");
  } else if (transitioning) {
    if (transitionSince === null) transitionSince = Date.now();
    const stuck = s.phase === "ready" && Date.now() - transitionSince > 12000;
    if (stuck) {
      els.restartBtn.disabled = false;
      els.restartBtn.textContent = T("已停止");
    } else {
      els.restartBtn.disabled = true;
      els.restartBtn.textContent = T("正在重启…");
    }
  } else if (queued) {
    transitionSince = null;
    els.restartBtn.disabled = true;
    els.restartBtn.textContent = T("正在重启…");
  } else {
    transitionSince = null;
    els.restartBtn.disabled = false;
    els.restartBtn.textContent = T("重启服务");
  }
}

async function loadSettings() {
  try {
    const s = await invoke<SettingsData>("get_settings");
    lastService = s;
    renderRestartButton(s);
    els.autostart.checked = s.autostart_enabled;
    setRowText(els.serviceState, serviceStateText(s));
    setRowText(els.appVersion, s.app_version);
    setRowText(els.dshVersion, s.dsh_version ?? T("尚未安装"));
    setRowText(els.nodeVersion, s.node_version ?? T("未知"));
    setRowText(els.workspacePath, s.workspace_dir);
    setRowText(els.logPath, s.log_file);

    configCache = await invoke<AppConfig>("get_config");
    els.autoUpdate.checked = configCache.auto_update_dsh;
    els.portInput.value = configCache.port > 0 ? String(configCache.port) : "0";
    els.registrySelect.value = configCache.registry_source || "auto";

    await loadUpdateStatus();

    await loadAppUpdateStatus();
  } catch (e) {
    setRowText(els.serviceState, T("读取失败"));
    console.error("load settings failed", e);
  }
}

let refreshing = false;

async function refreshSettings() {
  if (refreshing) return;
  refreshing = true;
  try {
    await loadSettings();
  } finally {
    refreshing = false;
  }
}

async function refreshServiceState() {
  try {
    const s = await invoke<ServiceState>("get_service_state");
    lastService = s;
    setRowText(els.serviceState, serviceStateText(s));
    renderRestartButton(s);
  } catch (e) {
    console.error("service state poll failed", e);
  }
}

function startServiceStatePoll() {
  setInterval(() => {
    if (document.visibilityState === "hidden") return;
    refreshServiceState();
  }, 1000);
}

async function bindRefreshEvents() {

  await listen("settings://refresh", refreshSettings);
  await listen("boot://progress", refreshSettings);
  await listen("boot://ready", refreshSettings);
  await listen("boot://error-options", refreshSettings);
}

function bind() {
  els.autostart.addEventListener("change", async () => {
    try {
      await invoke("set_autostart", { enabled: els.autostart.checked });
    } catch (e) {
      console.error("autostart toggle failed", e);
      els.autostart.checked = !els.autostart.checked;
      alert(lang === "zh" ? "切换开机启动失败，请稍后再试。" : "Failed to change startup setting. Please try again.");
    }
  });

  els.advancedSaveBtn.addEventListener("click", async () => {
    if (!configCache) return;
    const raw = els.portInput.value.trim();
    let port = Number(raw);
    if (raw === "" || Number.isNaN(port) || port < 0 || port > 65535) {
      alert(lang === "zh" ? "端口需为 0–65535 的数字。" : "Port must be a number between 0 and 65535.");
      return;
    }
    port = Math.trunc(port);
    const next = {
      ...configCache,
      port,
      registry_source: els.registrySelect.value,
    };
    try {
      await invoke("set_config", { config: next });
      configCache = next;
      setRowText(els.advancedSaveState, lang === "zh" ? "已保存（重启后生效）" : "Saved (applies after restart)");
      setTimeout(() => setRowText(els.advancedSaveState, ""), 3000);
    } catch (e) {
      console.error("advanced save failed", e);
      alert(lang === "zh" ? "保存失败，请稍后再试。" : "Failed to save. Please try again.");
    }
  });

  els.autoUpdate.addEventListener("change", async () => {
    if (!configCache) return;
    const next = { ...configCache, auto_update_dsh: els.autoUpdate.checked };
    try {
      await invoke("set_config", { config: next });
      configCache = next;
    } catch (e) {
      console.error("auto-update toggle failed", e);
      els.autoUpdate.checked = !els.autoUpdate.checked;
      alert(lang === "zh" ? "切换自动更新失败，请稍后再试。" : "Failed to change auto-update setting. Please try again.");
    }
  });

  els.restartBtn.addEventListener("click", async () => {
    if (restartBusy) return;
    restartBusy = true;
    els.restartBtn.disabled = true;
    els.restartBtn.textContent = lang === "zh" ? "正在重启…" : "Restarting…";
    try {
      await invoke("restart_service");
      await loadSettings();
    } catch (e) {
      alert(lang === "zh" ? "重启服务失败，请查看日志。" : "Failed to restart the service. Check the logs.");
      console.error("restart failed", e);
      await refreshSettings();
    } finally {
      restartBusy = false;
      if (lastService) {
        renderRestartButton(lastService);
      } else {
        els.restartBtn.disabled = false;
        els.restartBtn.textContent = T("重启服务");
      }
    }
  });

  els.checkUpdateBtn.addEventListener("click", doCheckUpdate);
  els.applyUpdateBtn.addEventListener("click", doApplyUpdate);

  els.appCheckUpdateBtn.addEventListener("click", doCheckAppUpdate);
  els.appApplyUpdateBtn.addEventListener("click", doApplyAppUpdate);

  els.openWorkspaceBtn.addEventListener("click", async () => {
    try {
      await invoke("open_workspace_dir");
    } catch (e) {
      alert(lang === "zh" ? "无法打开工作区。" : "Cannot open the workspace folder.");
    }
  });

  els.openLogBtn.addEventListener("click", async () => {
    try {
      await invoke("open_log_dir");
    } catch (e) {
      alert(lang === "zh" ? "无法打开日志目录。" : "Cannot open the logs folder.");
    }
  });
}

bind();
loadSettings();
bindRefreshEvents();
startServiceStatePoll();
