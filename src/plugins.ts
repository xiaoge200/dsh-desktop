/**
 * 插件页（FR-18）：DeepSeek Harness 插件管理器 + 应用市场。
 * 独立脚本模块，由 settings.html 以第二个 <script type="module"> 加载。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { SETTINGS_STRINGS, detectLang, tr } from "./i18n";

const lang = detectLang();
const dict = SETTINGS_STRINGS[lang];
const T = (k: string) => tr(dict, k);

interface PluginRow {
  id: string;
  name: string | null;
  enabled: boolean;
  managed: boolean;
  home_layer: boolean;
}

interface PluginInfo {
  name: string;
  version: string;
  spec: string;
  source: string;
  builtin: boolean;
  is_bundle: boolean;
  rows: PluginRow[];
  restart_required: boolean;
}

interface PluginsListData {
  plugins: PluginInfo[];
  service_restart_required: boolean;
  profile_dir: string;
  initialized: boolean;
}

interface PluginAddResult {
  name: string;
  version: string;
  restart_required: boolean;
  warning: string | null;
}

interface MarketplaceItem {
  name: string;
  description: string;
  source: string;
  spec: string;
  url: string;
}

interface MarketplaceResult {
  items: MarketplaceItem[];
  errors: string[];
}

const $ = <T extends HTMLElement = HTMLElement>(sel: string) => document.querySelector(sel) as T;

const els = {
  tabInstalled: $("#tab-installed") as HTMLButtonElement,
  tabMarket: $("#tab-market") as HTMLButtonElement,
  paneInstalled: $("#pane-installed"),
  paneMarket: $("#pane-market"),
  statusLine: $("#plugin-status"),
  list: $("#plugin-list"),
  specInput: $("#plugin-spec-input") as HTMLInputElement,
  addBtn: $("#plugin-add-btn") as HTMLButtonElement,
  searchInput: $("#market-search-input") as HTMLInputElement,
  refreshBtn: $("#market-refresh-btn") as HTMLButtonElement,
  marketErrors: $("#market-errors"),
  marketList: $("#market-list"),
};

function el(tag: string, className?: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function setStatus(text: string) {
  if (!text) {
    els.statusLine.hidden = true;
    els.statusLine.textContent = "";
  } else {
    els.statusLine.hidden = false;
    els.statusLine.textContent = text;
  }
}

function sourceLabel(p: PluginInfo): string {
  switch (p.source) {
    case "git":
      return T("Git");
    case "file":
      return T("本地");
    default:
      return "npm";
  }
}

// ---------------------------------------------------------------------------
// 已安装页
// ---------------------------------------------------------------------------

async function renderList() {
  try {
    const data = await invoke<PluginsListData>("plugins_list");
    els.list.replaceChildren();
    if (!data.initialized) {
      els.list.appendChild(el("div", "hint", T("尚未初始化插件")));
      setStatus("");
      return;
    }
    if (data.plugins.length === 0) {
      els.list.appendChild(el("div", "hint", T("尚未安装插件")));
      setStatus("");
      return;
    }
    for (const p of data.plugins) {
      els.list.appendChild(renderPlugin(p));
    }
    setStatus(data.service_restart_required ? T("有插件需要重启服务后生效") : "");
  } catch (e) {
    setStatus(T("插件加载失败"));
    console.error("plugins_list failed", e);
  }
}

function renderPlugin(p: PluginInfo): HTMLElement {
  const box = el("div", "plugin-entry");
  const head = el("div", "plugin-head");

  const info = el("div", "plugin-info");
  const title = el("div", "plugin-title");
  title.appendChild(el("span", "plugin-name", p.name));
  title.appendChild(el("span", "plugin-version", p.version));
  info.appendChild(title);

  const badges = el("div", "plugin-badges");
  badges.appendChild(el("span", `badge badge-${p.builtin ? "builtin" : p.source}`, p.builtin ? T("内置") : sourceLabel(p)));
  if (!p.builtin && !p.is_bundle) {
    badges.appendChild(el("span", "badge badge-plain", T("普通依赖")));
  }
  if (p.restart_required) {
    badges.appendChild(el("span", "badge badge-restart", T("重启后生效")));
  }
  info.appendChild(badges);
  head.appendChild(info);

  if (!p.builtin) {
    const removeBtn = el("button", "btn btn-small", T("移除")) as HTMLButtonElement;
    removeBtn.addEventListener("click", async () => {
      if (!confirm(`${T("确定要移除插件？")} ${p.name}`)) return;
      removeBtn.disabled = true;
      try {
        await invoke("plugins_remove", { name: p.name });
        await renderList();
        setStatus(T("已移除，重启服务后生效"));
      } catch (e) {
        alert(String(e));
      } finally {
        removeBtn.disabled = false;
      }
    });
    head.appendChild(removeBtn);
  }
  box.appendChild(head);

  for (const r of p.rows) {
    box.appendChild(renderRow(r));
  }
  return box;
}

function renderRow(r: PluginRow): HTMLElement {
  const row = el("div", "plugin-row");
  const text = el("div", "row-text");
  const label = el("span", "label", r.name ?? r.id);
  const hint = el("span", "hint", r.id);
  text.append(label, hint);
  row.appendChild(text);

  if (!r.managed) {
    const locked = el("span", "hint", T("在 home 层禁用"));
    locked.title = "~/.dsh/cordis.patch.yml";
    row.appendChild(locked);
  } else {
    const sw = el("label", "switch switch-small");
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = r.enabled;
    input.addEventListener("change", async () => {
      input.disabled = true;
      try {
        await invoke("plugins_set_enabled", { id: r.id, enabled: input.checked });
        await renderList();
      } catch (e) {
        console.error("plugins_set_enabled failed", e);
        alert(String(e));
        await renderList();
      } finally {
        input.disabled = false;
      }
    });
    sw.appendChild(input);
    sw.appendChild(el("span", "slider"));
    row.appendChild(sw);
  }
  return row;
}

/** 是否属于第三方代码来源（安装前弹确认） */
function isThirdParty(spec: string): boolean {
  const s = spec.trim();
  return (
    s.startsWith("github:") ||
    s.startsWith("git+") ||
    s.startsWith("git://") ||
    s.startsWith("file:") ||
    (s.includes("/") && !s.startsWith("@")) ||
    /^[A-Za-z]:[\\/]/.test(s) ||
    s.startsWith("/")
  );
}

async function doAdd(spec: string) {
  const btn = els.addBtn;
  btn.disabled = true;
  btn.textContent = T("正在安装…");
  try {
    const res = await invoke<PluginAddResult>("plugins_add", { spec });
    await renderList();
    const parts: string[] = [];
    if (res.warning) parts.push(res.warning);
    parts.push(T("已安装，重启服务后生效"));
    setStatus(parts.join(" "));
    els.specInput.value = "";
  } catch (e) {
    alert(String(e));
  } finally {
    btn.disabled = false;
    btn.textContent = T("安装");
  }
}

// ---------------------------------------------------------------------------
// 应用市场页
// ---------------------------------------------------------------------------

async function renderMarket() {
  els.marketList.replaceChildren(el("div", "hint", T("加载中…")));
  try {
    const search = els.searchInput.value.trim();
    const res = await invoke<MarketplaceResult>("plugins_marketplace", { search: search || null });
    els.marketErrors.hidden = res.errors.length === 0;
    els.marketErrors.textContent = res.errors.join(" ");
    els.marketList.replaceChildren();
    if (res.items.length === 0) {
      els.marketList.appendChild(el("div", "hint", T("没有匹配的插件")));
      return;
    }
    for (const item of res.items) {
      els.marketList.appendChild(renderMarketItem(item));
    }
  } catch (e) {
    els.marketList.replaceChildren();
    els.marketErrors.hidden = false;
    els.marketErrors.textContent = String(e);
    console.error("plugins_marketplace failed", e);
  }
}

function renderMarketItem(item: MarketplaceItem): HTMLElement {
  const box = el("div", "market-item");
  const info = el("div", "market-info");
  const title = el("div", "market-title");
  title.appendChild(el("span", "plugin-name", item.name));
  title.appendChild(
    el("span", `badge badge-${item.source}`, item.source === "github" ? "GitHub" : "npm"),
  );
  info.appendChild(title);
  if (item.description) {
    const desc = el("div", "hint", item.description);
    desc.title = item.url;
    info.appendChild(desc);
  }
  box.appendChild(info);

  const btn = el("button", "btn btn-small", T("安装")) as HTMLButtonElement;
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    btn.textContent = T("正在安装…");
    try {
      if (isThirdParty(item.spec) && !confirm(T("安装第三方插件确认"))) {
        return;
      }
      await doAdd(item.spec);
      switchTab("installed");
    } finally {
      btn.disabled = false;
      btn.textContent = T("安装");
    }
  });
  box.appendChild(btn);
  return box;
}

function switchTab(which: "installed" | "market") {
  const market = which === "market";
  els.tabInstalled.classList.toggle("active", !market);
  els.tabMarket.classList.toggle("active", market);
  els.paneInstalled.hidden = market;
  els.paneMarket.hidden = !market;
  if (market) {
    renderMarket();
  }
}

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------

function applyPlaceholders() {
  document.querySelectorAll<HTMLInputElement>("[data-i18n-placeholder]").forEach((input) => {
    const key = input.getAttribute("data-i18n-placeholder");
    if (key) input.placeholder = T(key);
  });
}

function bind() {
  els.tabInstalled.addEventListener("click", () => switchTab("installed"));
  els.tabMarket.addEventListener("click", () => switchTab("market"));

  els.addBtn.addEventListener("click", async () => {
    const spec = els.specInput.value.trim();
    if (!spec) {
      alert(T("插件输入提示"));
      return;
    }
    if (isThirdParty(spec) && !confirm(T("安装第三方插件确认"))) {
      return;
    }
    await doAdd(spec);
  });

  els.refreshBtn.addEventListener("click", () => renderMarket());
  els.searchInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      renderMarket();
    }
  });
}

applyPlaceholders();
bind();
renderList();

// 与设置页其他数据一样：窗口在启动时即创建（hidden），页面加载早于
// 插件环境初始化（默认插件安装是后台异步完成的），首次渲染可能是
// "尚未初始化插件"。窗口每次打开时（settings://refresh）重新拉取列表。
listen("settings://refresh", () => renderList());
