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
  dsh_version: string | null;
  node_version: string | null;
}

const els = {
  title: document.querySelector("#title") as HTMLElement,
  message: document.querySelector("#message") as HTMLElement,
  progressBar: document.querySelector("#progress-bar") as HTMLElement,
  progressTrack: document.querySelector("#progress-track") as HTMLElement,
  errorBox: document.querySelector("#error-box") as HTMLElement,
  errorMessage: document.querySelector("#error-message") as HTMLElement,
  techDetail: document.querySelector("#tech-detail") as HTMLElement,
  repairBtn: document.querySelector("#repair-btn") as HTMLButtonElement,
  retryBtn: document.querySelector("#retry-btn") as HTMLButtonElement,
  quitBtn: document.querySelector("#quit-btn") as HTMLButtonElement,
  welcomeBox: document.querySelector("#welcome-box") as HTMLElement,
  welcomeBtn: document.querySelector("#welcome-btn") as HTMLButtonElement,
};

// 语言（NFR-10）：中文环境显示中文，其余英文
const lang = detectLang();
const dict = BOOT_STRINGS[lang];
const T = (k: string) => tr(dict, k);

// 阶段 → 白话标题 + 进度百分比
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
  // 一键修复（FR-12）：服务类故障可修复时显示修复按钮
  els.repairBtn.classList.remove("hidden");
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
  // 就绪后跳转到本地 DSH 服务
  window.location.href = url;
}

// 首次使用引导（FR-16）：READY 后先显示白话引导，点「开始使用」才进入；
// 用 localStorage 记录，仅首次显示。
const WELCOME_KEY = "dsh-desktop.welcome-seen";
let pendingUrl: string | null = null;

function showWelcome(url: string) {
  pendingUrl = url;
  els.title.textContent = T("准备完成");
  els.message.classList.add("hidden");
  els.progressTrack.classList.add("hidden");
  els.welcomeBox.classList.remove("hidden");
  // 引导文案（FR-16 + NFR-10）
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
    // 一键修复（FR-12）：重建运行时 + 重启服务；期间显示修复中状态
    els.repairBtn.disabled = true;
    els.repairBtn.textContent = lang === "zh" ? "正在修复…" : "Repairing…";
    els.errorMessage.textContent = lang === "zh" ? "正在修复，请稍候…" : "Repairing, please wait…";
    els.techDetail.textContent = "";
    try {
      await invoke("repair_service");
      // 修复成功后 boot://ready 事件会触发跳转；这里兜底 reload
    } catch (e) {
      els.errorMessage.textContent =
        lang === "zh" ? "修复没有成功，请稍后再试或重新安装。" : "Repair failed. Please try again or reinstall.";
      console.error("repair failed", e);
    } finally {
      els.repairBtn.disabled = false;
      els.repairBtn.textContent = lang === "zh" ? "一键修复" : "Repair";
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

  // 事件监听
  await listen<BootProgress>("boot://progress", (e) => {
    setPhase(e.payload.phase, e.payload.message);
  });
  await listen<BootReady>("boot://ready", (e) => {
    handleReady(e.payload.url);
  });

  // 先拉一次快照（若 boot 已完成或已失败）
  try {
    const snap = await invoke<StatusSnapshot>("get_status");
    if (snap.phase === "ready" && snap.port > 0) {
      handleReady(`http://127.0.0.1:${snap.port}`);
      return;
    }
    if (snap.phase === "error") {
      showError(snap.error ?? "遇到了一点问题，请重试。");
      return;
    }
    setPhase(snap.phase, snap.message);
  } catch (e) {
    // 忽略：事件流会接管
  }
}

function handleReady(url: string) {
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
