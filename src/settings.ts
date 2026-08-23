import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

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
  closeBtn: $("#close-btn") as HTMLButtonElement,
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
      s.port > 0 ? `运行中（端口 ${s.port}）` : "未启动",
    );
    setRowText(els.appVersion, s.app_version);
    setRowText(els.dshVersion, s.dsh_version ?? "尚未安装");
    setRowText(els.nodeVersion, s.node_version ?? "未知");
    setRowText(els.workspacePath, s.workspace_dir);
    setRowText(els.logPath, s.log_file);

    // 自动更新开关
    configCache = await invoke<AppConfig>("get_config");
    els.autoUpdate.checked = configCache.auto_update_dsh;
  } catch (e) {
    setRowText(els.serviceState, "读取失败");
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
      alert("切换开机启动失败，请稍后再试。");
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
      alert("切换自动更新失败，请稍后再试。");
    }
  });

  els.restartBtn.addEventListener("click", async () => {
    els.restartBtn.disabled = true;
    els.restartBtn.textContent = "正在重启…";
    try {
      await invoke("restart_service");
      await loadSettings();
    } catch (e) {
      alert("重启服务失败，请查看日志。");
      console.error("restart failed", e);
    } finally {
      els.restartBtn.disabled = false;
      els.restartBtn.textContent = "重启服务";
    }
  });

  els.openWorkspaceBtn.addEventListener("click", async () => {
    try {
      await invoke("open_workspace_dir");
    } catch (e) {
      alert("无法打开工作区。");
    }
  });

  els.openLogBtn.addEventListener("click", async () => {
    try {
      await invoke("open_log_dir");
    } catch (e) {
      alert("无法打开日志目录。");
    }
  });

  els.closeBtn.addEventListener("click", () => {
    getCurrentWindow().close();
  });
}

bind();
loadSettings();
