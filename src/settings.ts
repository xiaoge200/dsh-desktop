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

type RowKind = "dsh" | "app";

interface UpdateStageState {
  stage: string;
  can_cancel: boolean;
  received: number | null;
  total: number | null;
}

interface UpdateProgressSnapshot {
  dsh: UpdateStageState;
  app: UpdateStageState;
}

const idleStage = (): UpdateStageState => ({ stage: "idle", can_cancel: false, received: null, total: null });

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
  dshUpdateBtn: $("#dsh-update-btn") as HTMLButtonElement,
  appUpdateState: $("#app-update-state"),
  appUpdateBtn: $("#app-update-btn") as HTMLButtonElement,
};

let configCache: AppConfig | null = null;
let lastService: ServiceState | null = null;
let restartBusy = false;
let transitionSince: number | null = null;

function setRowText(el: HTMLElement, text: string) {
  el.textContent = text;
}

let lastUpdateStatus: DshUpdateStatus | null = null;
let lastAppUpdateStatus: DshUpdateStatus | null = null;
let lastProgress: UpdateProgressSnapshot = { dsh: idleStage(), app: idleStage() };
const actionBusy: Record<RowKind, boolean> = { dsh: false, app: false };

function rowStatus(kind: RowKind): DshUpdateStatus | null {
  return kind === "dsh" ? lastUpdateStatus : lastAppUpdateStatus;
}

function rowEls(kind: RowKind): { btn: HTMLButtonElement; state: HTMLElement; version: HTMLElement } {
  return kind === "dsh"
    ? { btn: els.dshUpdateBtn, state: els.updateState, version: els.dshVersion }
    : { btn: els.appUpdateBtn, state: els.appUpdateState, version: els.appVersion };
}

function percentText(p: UpdateStageState): string | null {
  if (p.total && p.total > 0 && p.received !== null) {
    return `(${Math.round((p.received / p.total) * 100)}%)`;
  }
  return null;
}

function busyText(p: UpdateStageState): string {
  switch (p.stage) {
    case "checking":
      return T("检查中…");
    case "downloading": {
      const base = T("正在下载更新…");
      const pct = percentText(p);
      return pct ? `${base} ${pct}` : base;
    }
    case "swapping":
      return T("正在应用更新…");
    case "installing":
      return T("正在安装更新…");
    case "restarting":
      return T("正在重启服务…");
    default:
      return "";
  }
}

function idleMessage(kind: RowKind, st: DshUpdateStatus): string {
  let msg = st.message;
  if (kind === "dsh") {
    if (st.pre_available && st.prerelease && !st.update_available) {
      msg = lang === "zh"
        ? `已是最新正式版；发现预发布版本 ${st.prerelease}（已开启预发布更新）`
        : `Latest stable installed; prerelease ${st.prerelease} available (prerelease updates on)`;
    } else if (st.prerelease && st.pre_available) {
      msg = lang === "zh"
        ? `${st.message}（预发布 ${st.prerelease}）`
        : `${st.message} (prerelease ${st.prerelease})`;
    }
  }
  return msg;
}

function renderRow(kind: RowKind) {
  const { btn, state, version } = rowEls(kind);
  const p = lastProgress[kind];
  const st = rowStatus(kind);

  if (p.stage !== "idle") {
    btn.disabled = !(p.stage === "downloading" && p.can_cancel);
    if (p.stage === "downloading") {
      btn.textContent = p.can_cancel ? T("停止") : T("自动更新中…");
    } else {
      btn.textContent = busyText(p);
    }
    setRowText(state, busyText(p));
    return;
  }

  btn.disabled = actionBusy[kind];
  if (!st) {
    btn.textContent = T("检查更新");
    setRowText(state, T("尚未检查更新"));
    return;
  }
  if (st.update_available) {
    btn.textContent = T("立即更新");
    const target = st.latest ?? st.prerelease;
    if (target) setRowText(version, target);
  } else {
    btn.textContent = T("检查更新");
  }
  setRowText(state, idleMessage(kind, st));
}

async function loadUpdateStatus(kind: RowKind) {
  try {
    const cmd = kind === "dsh" ? "get_dsh_update_status" : "get_app_update_status";
    const status = await invoke<DshUpdateStatus | null>(cmd);
    if (kind === "dsh") lastUpdateStatus = status;
    else lastAppUpdateStatus = status;
  } catch (e) {
    console.error(`load ${kind} update status failed`, e);
    if (kind === "dsh") lastUpdateStatus = null;
    else lastAppUpdateStatus = null;
  }
  renderRow(kind);
}

async function loadProgress() {
  const wasBusy = lastProgress.dsh.stage !== "idle" || lastProgress.app.stage !== "idle";
  try {
    lastProgress = await invoke<UpdateProgressSnapshot>("get_update_progress");
  } catch (e) {
    console.error("load update progress failed", e);
  }
  renderRow("dsh");
  renderRow("app");
  const nowBusy = lastProgress.dsh.stage !== "idle" || lastProgress.app.stage !== "idle";
  if (wasBusy && !nowBusy) {
    void refreshSettings();
  }
}

async function refreshUpdateUI() {
  await Promise.all([loadProgress(), loadUpdateStatus("dsh"), loadUpdateStatus("app")]);
}

async function runRowAction(kind: RowKind) {
  if (actionBusy[kind] || lastProgress[kind].stage !== "idle") return;
  const st = rowStatus(kind);
  const doUpdate = !!st && st.update_available;
  const cmd = kind === "dsh"
    ? doUpdate ? "update_dsh" : "check_dsh_update"
    : doUpdate ? "update_app" : "check_app_update";
  actionBusy[kind] = true;
  renderRow(kind);
  try {
    await invoke<DshUpdateStatus>(cmd);
  } catch (e) {
    console.error(`${kind} ${doUpdate ? "update" : "check"} failed`, e);
    const msg = typeof e === "string" && e
      ? e
      : lang === "zh"
        ? doUpdate ? "更新失败，请查看日志。" : "检查更新失败，请查看日志。"
        : doUpdate ? "Update failed. See the logs." : "Failed to check updates. See the logs.";
    const prev = rowStatus(kind);
    const failed: DshUpdateStatus = {
      ok: false,
      update_available: prev?.update_available ?? false,
      current: prev?.current ?? null,
      latest: prev?.latest ?? null,
      prerelease: prev?.prerelease ?? null,
      pre_available: prev?.pre_available ?? false,
      message: msg,
    };
    if (kind === "dsh") lastUpdateStatus = failed;
    else lastAppUpdateStatus = failed;
    renderRow(kind);
  } finally {
    actionBusy[kind] = false;
    await refreshUpdateUI();
  }
}

async function stopRowAction(kind: RowKind) {
  const cmd = kind === "dsh" ? "cancel_dsh_update" : "cancel_app_update";
  try {
    await invoke(cmd);
  } catch (e) {
    console.error(`${kind} cancel failed`, e);
  }
  await refreshUpdateUI();
}

function onRowClick(kind: RowKind) {
  const p = lastProgress[kind];
  if (p.stage === "downloading" && p.can_cancel) {
    stopRowAction(kind);
    return;
  }
  runRowAction(kind);
}

function startUpdateProgressPoll() {
  setInterval(() => {
    if (document.visibilityState === "hidden") return;
    loadProgress();
  }, 500);
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

    await refreshUpdateUI();
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

  els.dshUpdateBtn.addEventListener("click", () => onRowClick("dsh"));
  els.appUpdateBtn.addEventListener("click", () => onRowClick("app"));

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
startUpdateProgressPoll();
