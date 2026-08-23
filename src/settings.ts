import { invoke } from "@tauri-apps/api/core";
import { SETTINGS_STRINGS, detectLang, tr } from "./i18n";

// 语言（NFR-10）
const lang = detectLang();
const dict = SETTINGS_STRINGS[lang];
const T = (k: string) => tr(dict, k);

// 应用翻译到静态 HTML 文本（data-i18n 标记的元素）
function applyStaticI18n() {
  document.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    const key = el.getAttribute("data-i18n");
    if (key) el.textContent = T(key);
  });
}
applyStaticI18n();

interface SettingsData {
  app_version: string;
  node_version: string | null;
  dsh_version: string | null;
  port: number;
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
};

let configCache: AppConfig | null = null;

function setRowText(el: HTMLElement, text: string) {
  el.textContent = text;
}

async function loadSettings() {
  try {
    const s = await invoke<SettingsData>("get_settings");
    els.autostart.checked = s.autostart_enabled;
    setRowText(
      els.serviceState,
      s.port > 0 ? (lang === "zh" ? `运行中（端口 ${s.port}）` : `Running (port ${s.port})`) : T("未启动"),
    );
    setRowText(els.appVersion, s.app_version);
    setRowText(els.dshVersion, s.dsh_version ?? T("尚未安装"));
    setRowText(els.nodeVersion, s.node_version ?? T("未知"));
    setRowText(els.workspacePath, s.workspace_dir);
    setRowText(els.logPath, s.log_file);

    // 自动更新开关
    configCache = await invoke<AppConfig>("get_config");
    els.autoUpdate.checked = configCache.auto_update_dsh;
    // 高级区：端口 + 更新源
    els.portInput.value = configCache.port > 0 ? String(configCache.port) : "0";
    els.registrySelect.value = configCache.registry_source || "auto";
  } catch (e) {
    setRowText(els.serviceState, T("读取失败"));
    console.error("load settings failed", e);
  }
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

  // 高级区保存（端口 + 更新源）
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
    els.restartBtn.disabled = true;
    els.restartBtn.textContent = lang === "zh" ? "正在重启…" : "Restarting…";
    try {
      await invoke("restart_service");
      await loadSettings();
    } catch (e) {
      alert(lang === "zh" ? "重启服务失败，请查看日志。" : "Failed to restart the service. Check the logs.");
      console.error("restart failed", e);
    } finally {
      els.restartBtn.disabled = false;
      els.restartBtn.textContent = T("重启服务");
    }
  });

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
