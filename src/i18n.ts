/**
 * 轻量 i18n（NFR-10）：按浏览器语言返回中/英文文案。
 * 中文环境（zh-*）显示中文，其余显示英文。
 */

export type Lang = "zh" | "en";

export function detectLang(): Lang {
  try {
    const nav = navigator.language || (navigator as unknown as { userLanguage?: string }).userLanguage || "";
    return nav.toLowerCase().startsWith("zh") ? "zh" : "en";
  } catch {
    return "zh";
  }
}

export const BOOT_STRINGS = {
  zh: {
    "正在准备程序…": "正在准备程序…",
    "检查运行环境，很快就好。": "检查运行环境，很快就好。",
    "正在准备首次使用…": "正在准备首次使用…",
    "下载所需组件，请稍候。第一次会慢一点，之后打开就快了。":
      "下载所需组件，请稍候。第一次会慢一点，之后打开就快了。",
    "正在启动…": "正在启动…",
    "启动本地服务，马上就好。": "启动本地服务，马上就好。",
    "准备完成": "准备完成",
    "即将打开工作台。": "即将打开工作台。",
    "遇到了一点问题": "遇到了一点问题",
    "请稍候。": "请稍候。",
    "（无技术详情）": "（无技术详情）",
    "欢迎使用 DSH 工作台": "欢迎使用 DSH 工作台",
    "这就是你的 AI 工作台，直接开始对话即可。\n所有内容都保存在这台电脑上，随时可用。":
      "这就是你的 AI 工作台，直接开始对话即可。\n所有内容都保存在这台电脑上，随时可用。",
    "开始使用": "开始使用",
    "一键修复": "一键修复",
    "正在修复…": "正在修复…",
  },
  en: {
    "正在准备程序…": "Preparing…",
    "检查运行环境，很快就好。": "Checking the environment. This won't take long.",
    "正在准备首次使用…": "Preparing first launch…",
    "下载所需组件，请稍候。第一次会慢一点，之后打开就快了。":
      "Downloading components. The first launch takes a little longer; after that it opens quickly.",
    "正在启动…": "Starting…",
    "启动本地服务，马上就好。": "Starting the local service, almost ready.",
    "准备完成": "Ready",
    "即将打开工作台。": "Opening your workspace…",
    "遇到了一点问题": "Something went wrong",
    "请稍候。": "Please wait.",
    "（无技术详情）": "(no technical details)",
    "欢迎使用 DSH 工作台": "Welcome to DSH Workspace",
    "这就是你的 AI 工作台，直接开始对话即可。\n所有内容都保存在这台电脑上，随时可用。":
      "This is your AI workspace. Just start chatting.\nEverything is stored on this computer and ready when you are.",
    "开始使用": "Get Started",
    "一键修复": "Repair",
    "正在修复…": "Repairing…",
  },
} as const;

export const SETTINGS_STRINGS = {
  zh: {
    "设置": "设置",
    "启动": "启动",
    "开机自动启动": "开机自动启动",
    "电脑开机后自动打开 DSH 工作台": "电脑开机后自动打开 DSH 工作台",
    "自动更新": "自动更新",
    "有新版本时自动更新，让你始终用上最新功能": "有新版本时自动更新，让你始终用上最新功能",
    "服务": "服务",
    "本地服务": "本地服务",
    "正在检查…": "正在检查…",
    "重启服务": "重启服务",
    "关于": "关于",
    "应用版本": "应用版本",
    "DSH 版本": "DSH 版本",
    "运行环境": "运行环境",
    "工作区位置": "工作区位置",
    "日志位置": "日志位置",
    "打开工作区": "打开工作区",
    "打开日志": "打开日志",
    "高级": "高级",
    "本地端口": "本地端口",
    "端口提示": "端口被占用时会自动更换；0 表示自动",
    "更新源": "更新源",
    "更新源提示": "国内网络建议选择国内镜像",
    "自动": "自动",
    "官方源": "官方源",
    "国内镜像": "国内镜像",
    "保存": "保存",
    "读取失败": "读取失败",
    "尚未安装": "尚未安装",
    "未知": "未知",
    "未启动": "未启动",
  },
  en: {
    "设置": "Settings",
    "启动": "Startup",
    "开机自动启动": "Launch at startup",
    "电脑开机后自动打开 DSH 工作台": "Open DSH Workspace automatically when your computer starts",
    "自动更新": "Auto-update",
    "有新版本时自动更新，让你始终用上最新功能": "Update automatically when a new version is available",
    "服务": "Service",
    "本地服务": "Local service",
    "正在检查…": "Checking…",
    "重启服务": "Restart service",
    "关于": "About",
    "应用版本": "App version",
    "DSH 版本": "DSH version",
    "运行环境": "Runtime",
    "工作区位置": "Workspace",
    "日志位置": "Logs",
    "打开工作区": "Open workspace",
    "打开日志": "Open logs",
    "高级": "Advanced",
    "本地端口": "Local port",
    "端口提示": "Switches automatically if occupied; 0 = auto",
    "更新源": "Update source",
    "更新源提示": "Choose the mirror if you're in mainland China",
    "自动": "Auto",
    "官方源": "Official",
    "国内镜像": "Mirror (CN)",
    "保存": "Save",
    "读取失败": "Failed to load",
    "尚未安装": "Not installed",
    "未知": "Unknown",
    "未启动": "Not running",
  },
} as const;

/** 用当前语言翻译 key（若 key 无英文翻译则原样返回） */
export function tr(dict: Record<string, string>, key: string): string {
  return dict[key] ?? key;
}
