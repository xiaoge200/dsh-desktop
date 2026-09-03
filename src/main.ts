import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { BOOT_STRINGS, detectLang, tr } from "./i18n";

interface BootProgress {
  phase: string;
  message: string;
}

interface BootReady {
  url: string;
}

interface StatusSnapshot {
  phase: string;
  message: string;
  error: string | null;
  port: number;
  service_url: string | null;
  recovery: BootErrorOptions | null;
  dsh_version: string | null;
  node_version: string | null;
}

interface BootErrorOptions {
  kind: "plugins" | "stale-lock" | "unknown";
  plugins: string[];
  lock_path?: string | null;
  detail?: string;
}

const els = {
  title: document.querySelector("#title") as HTMLElement,
  message: document.querySelector("#message") as HTMLElement,
  progressBar: document.querySelector("#progress-bar") as HTMLElement,
  progressTrack: document.querySelector("#progress-track") as HTMLElement,
  errorBox: document.querySelector("#error-box") as HTMLElement,
  errorMessage: document.querySelector("#error-message") as HTMLElement,
  recoveryDesc: document.querySelector("#recovery-desc") as HTMLElement,
  badPluginList: document.querySelector("#bad-plugin-list") as HTMLUListElement,
  removePluginsBtn: document.querySelector("#remove-plugins-btn") as HTMLButtonElement,
  techDetail: document.querySelector("#tech-detail") as HTMLElement,
  repairBtn: document.querySelector("#repair-btn") as HTMLButtonElement,
  retryBtn: document.querySelector("#retry-btn") as HTMLButtonElement,
  quitBtn: document.querySelector("#quit-btn") as HTMLButtonElement,
  welcomeBox: document.querySelector("#welcome-box") as HTMLElement,
  welcomeBtn: document.querySelector("#welcome-btn") as HTMLButtonElement,
};

let recoveryOptions: BootErrorOptions | null = null;

const lang = detectLang();
const dict = BOOT_STRINGS[lang];
const T = (k: string) => tr(dict, k);

const PHASE_TEXT: Record<string, { title: string; message: string; pct: number }> = {
  "node-check": {
    title: T("正在准备程序…"),
    message: T("检查运行环境，很快就好。"),
    pct: 15,
  },
  "dsh-install": {
    title: T("正在准备首次使用…"),
    message: T("下载所需组件，请稍候。第一次会慢一点，之后打开就快了。"),
    pct: 45,
  },
  "service-start": {
    title: T("正在启动…"),
    message: T("启动本地服务，马上就好。"),
    pct: 75,
  },
  ready: {
    title: T("准备完成"),
    message: T("即将打开工作台。"),
    pct: 100,
  },
  error: {
    title: T("遇到了一点问题"),
    message: "",
    pct: 0,
  },
};

function renderRecovery() {
  els.recoveryDesc.classList.add("hidden");
  els.badPluginList.replaceChildren();
  els.removePluginsBtn.classList.add("hidden");
  els.repairBtn.classList.remove("hidden");
  const opts = recoveryOptions;
  if (!opts) return;
  if (opts.kind === "plugins" && opts.plugins.length > 0) {
    els.errorMessage.textContent = T("部分已安装的插件与当前 DSH 版本不兼容，导致服务无法启动。");
    for (const name of opts.plugins) {
      const li = document.createElement("li");
      li.textContent = name;
      els.badPluginList.appendChild(li);
    }
    els.badPluginList.classList.remove("hidden");
    els.removePluginsBtn.classList.remove("hidden");
    els.repairBtn.classList.add("hidden");
  } else if (opts.kind === "stale-lock" && opts.lock_path) {
    els.errorMessage.textContent = T("残留的锁文件仍在阻止服务启动，可手动删除后重试：");
    els.recoveryDesc.textContent = opts.lock_path;
    els.recoveryDesc.classList.remove("hidden");
  }
}

function clearRecovery() {
  recoveryOptions = null;
  els.recoveryDesc.classList.add("hidden");
  els.badPluginList.replaceChildren();
  els.badPluginList.classList.add("hidden");
  els.removePluginsBtn.classList.add("hidden");
  els.removePluginsBtn.disabled = false;
  els.removePluginsBtn.textContent = T("移除不兼容插件");
}

function showError(message: string, detail?: string) {
  els.title.textContent = T("遇到了一点问题");
  els.errorBox.classList.remove("hidden");
  els.progressTrack.classList.add("hidden");
  els.errorMessage.textContent = message;
  if (detail) {
    els.techDetail.textContent = detail;
  } else {
    els.techDetail.textContent = T("（无技术详情）");
  }
  renderRecovery();
}

function setPhase(phase: string, message?: string) {
  const t = PHASE_TEXT[phase] ?? {
    title: T("正在启动…"),
    message: message ?? T("请稍候。"),
    pct: 10,
  };
  els.title.textContent = t.title;
  els.message.textContent = message && message.length > 0 ? message : t.message;
  els.progressBar.style.width = `${t.pct}%`;
}

async function navigate(url: string) {
  try {
    const u = new URL(url, window.location.href);
    if (u.searchParams.has("token") && u.origin !== window.location.origin) {
      window.location.href = u.origin + u.pathname;
      return;
    }
  } catch {
  }
  window.location.href = url;
}

const WELCOME_KEY = "dsh-desktop.welcome-seen";
let pendingUrl: string | null = null;

function showWelcome(url: string) {
  pendingUrl = url;
  els.title.textContent = T("准备完成");
  els.message.classList.add("hidden");
  els.progressTrack.classList.add("hidden");
  els.welcomeBox.classList.remove("hidden");
  const welcomeHeading = els.welcomeBox.querySelector("h2");
  const welcomeText = els.welcomeBox.querySelector(".welcome-text");
  const welcomeBtnText = els.welcomeBtn;
  if (welcomeHeading) welcomeHeading.textContent = T("欢迎使用 DSH 工作台");
  if (welcomeText) welcomeText.textContent = T("这就是你的 AI 工作台，直接开始对话即可。\n所有内容都保存在这台电脑上，随时可用。");
  welcomeBtnText.textContent = T("开始使用");
}

function bindActions() {
  els.retryBtn.addEventListener("click", async () => {
    els.errorBox.classList.add("hidden");
    els.progressTrack.classList.remove("hidden");
    setPhase("node-check");
    window.location.reload();
  });
  els.repairBtn.addEventListener("click", async () => {
    els.repairBtn.disabled = true;
    els.repairBtn.textContent = lang === "zh" ? "正在修复…" : "Repairing…";
    els.errorMessage.textContent = lang === "zh" ? "正在修复，请稍候…" : "Repairing, please wait…";
    els.techDetail.textContent = "";
    try {
      await invoke("repair_service");
    } catch (e) {
      els.errorMessage.textContent =
        lang === "zh" ? "修复没有成功，请稍后再试或重新安装。" : "Repair failed. Please try again or reinstall.";
      console.error("repair failed", e);
    } finally {
      els.repairBtn.disabled = false;
      els.repairBtn.textContent = lang === "zh" ? "一键修复" : "Repair";
    }
  });
  els.removePluginsBtn.addEventListener("click", async () => {
    const opts = recoveryOptions;
    if (!opts) return;
    els.removePluginsBtn.disabled = true;
    els.removePluginsBtn.textContent = T("正在移除并重试…");
    els.errorMessage.textContent = T("正在移除不兼容插件，请稍候…");
    els.techDetail.textContent = "";
    try {
      const removed = await invoke<string[]>("plugins_remove_incompatible", {
        names: opts.plugins,
      });
      if (!removed || removed.length === 0) {
        els.errorMessage.textContent = T("没有需要移除的插件，请重试。");
        renderRecovery();
        return;
      }
      els.errorMessage.textContent = T("移除完成，正在重启服务…");
      await invoke("restart_service");
    } catch (e) {
      console.error("remove incompatible plugins failed", e);
      els.errorMessage.textContent = T("移除失败：") + String(e);
      renderRecovery();
    } finally {
      els.removePluginsBtn.disabled = false;
      els.removePluginsBtn.textContent = T("移除不兼容插件");
    }
  });
  els.quitBtn.addEventListener("click", async () => {
    getCurrentWindow().destroy();
  });
  els.welcomeBtn.addEventListener("click", () => {
    try {
      localStorage.setItem(WELCOME_KEY, "1");
    } catch { /* 忽略存储失败 */ }
    if (pendingUrl) navigate(pendingUrl);
  });
}

async function main() {
  bindActions();

  await listen<BootProgress>("boot://progress", (e) => {
    setPhase(e.payload.phase, e.payload.message);
  });
  await listen<BootReady>("boot://ready", (e) => {
    handleReady(e.payload.url);
  });
  await listen<BootErrorOptions>("boot://error-options", (e) => {
    recoveryOptions = e.payload;
    if (!els.errorBox.classList.contains("hidden")) {
      renderRecovery();
    }
  });

  try {
    const snap = await invoke<StatusSnapshot>("get_status");
    if (snap.phase === "ready" && snap.port > 0) {
      handleReady(snap.service_url ?? `http://127.0.0.1:${snap.port}`);
      return;
    }
    if (snap.phase === "error") {
      recoveryOptions = snap.recovery;
      showError(snap.error ?? "遇到了一点问题，请重试。");
      return;
    }
    setPhase(snap.phase, snap.message);
  } catch (e) {
  }
}

function handleReady(url: string) {
  clearRecovery();
  els.errorBox.classList.add("hidden");
  els.progressTrack.classList.remove("hidden");
  els.progressBar.style.width = "100%";
  els.title.textContent = T("准备完成");
  let seen = false;
  try {
    seen = localStorage.getItem(WELCOME_KEY) === "1";
  } catch { /* 忽略 */ }
  if (seen) {
    navigate(url);
  } else {
    showWelcome(url);
  }
}

main();
