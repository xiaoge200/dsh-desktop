import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

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
  retryBtn: document.querySelector("#retry-btn") as HTMLButtonElement,
  quitBtn: document.querySelector("#quit-btn") as HTMLButtonElement,
};

// 阶段 → 白话标题 + 进度百分比
const PHASE_TEXT: Record<string, { title: string; message: string; pct: number }> = {
  "node-check": {
    title: "正在准备程序…",
    message: "检查运行环境，很快就好。",
    pct: 15,
  },
  "dsh-install": {
    title: "正在准备首次使用…",
    message: "下载所需组件，请稍候。第一次会慢一点，之后打开就快了。",
    pct: 45,
  },
  "service-start": {
    title: "正在启动…",
    message: "启动本地服务，马上就好。",
    pct: 75,
  },
  ready: {
    title: "准备完成",
    message: "即将打开工作台。",
    pct: 100,
  },
  error: {
    title: "遇到了一点问题",
    message: "",
    pct: 0,
  },
};

function showError(message: string, detail?: string) {
  els.title.textContent = "遇到了一点问题";
  els.errorBox.classList.remove("hidden");
  els.progressTrack.classList.add("hidden");
  els.errorMessage.textContent = message;
  if (detail) {
    els.techDetail.textContent = detail;
  } else {
    els.techDetail.textContent = "（无技术详情）";
  }
}

function setPhase(phase: string, message?: string) {
  const t = PHASE_TEXT[phase] ?? {
    title: "正在启动…",
    message: message ?? "请稍候。",
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

function bindActions() {
  els.retryBtn.addEventListener("click", async () => {
    els.errorBox.classList.add("hidden");
    els.progressTrack.classList.remove("hidden");
    setPhase("node-check");
    window.location.reload();
  });
  els.quitBtn.addEventListener("click", async () => {
    getCurrentWindow().destroy();
  });
}

async function main() {
  bindActions();

  // 事件监听
  await listen<BootProgress>("boot://progress", (e) => {
    setPhase(e.payload.phase, e.payload.message);
  });
  await listen<BootReady>("boot://ready", (e) => {
    navigate(e.payload.url);
  });

  // 先拉一次快照（若 boot 已完成或已失败）
  try {
    const snap = await invoke<StatusSnapshot>("get_status");
    if (snap.phase === "ready" && snap.port > 0) {
      navigate(`http://127.0.0.1:${snap.port}`);
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

main();
