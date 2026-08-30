mod config;
mod logging;
mod node;
mod plugins;
mod state;
mod supervisor;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use state::{AppState, BootPhase, DshUpdateStatus, StatusSnapshot};
use supervisor::Supervisor;
use tauri::{AppHandle, Emitter, Manager, State};

/// 应用数据目录下的子目录
const RUNTIME_DIR: &str = "dsh-runtime";
const WORKSPACE_DIR: &str = "workspace";
const LOG_FILE: &str = "dsh-desktop.log";

/// 显示主窗口（从托盘/单实例等入口复用）
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 更新托盘 tooltip（FR-07：服务运行/降级状态提示）
fn update_tray_tooltip(app: &AppHandle) {
    let state = app.state::<AppState>();
    let phase = state.phase();
    let port = state.port();
    let degraded = state.supervisor.lock().unwrap().is_degraded();
    let zh = is_zh_locale();
    let tooltip: String = if phase == BootPhase::Ready {
        if degraded {
            if zh { "DSH 工作台：服务多次启动失败".into() } else { "DSH Workspace: service failed repeatedly".into() }
        } else if port > 0 {
            if zh { format!("DSH 工作台：运行中（端口 {port}）") } else { format!("DSH Workspace: running (port {port})") }
        } else {
            if zh { "DSH 工作台：正在启动…".into() } else { "DSH Workspace: starting…".into() }
        }
    } else if phase == BootPhase::Error {
        if zh { "DSH 工作台：遇到问题，请重试".into() } else { "DSH Workspace: something went wrong".into() }
    } else {
        if zh { "DSH 工作台：正在启动…".into() } else { "DSH Workspace: starting…".into() }
    };
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
}

/// 显示设置窗口（隐藏后再次打开仍有效——窗口关闭时是隐藏而非销毁）
fn show_settings_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    } else {
        log::warn!("settings window not found; creating");
        // 窗口意外被销毁（极端情况）：重新创建
        if let Err(e) = tauri::WebviewWindowBuilder::new(
            app,
            "settings",
            tauri::WebviewUrl::App("settings.html".into()),
        )
        .title("设置")
        .inner_size(560.0, 640.0)
        .resizable(false)
        .center()
        .build()
        {
            log::error!("create settings window failed: {e}");
        }
    }
    // 设置窗口在应用启动时即已创建（hidden），页面加载早于 boot 完成，
    // 首次拉取到的 DSH 版本/运行环境/工作区/服务状态可能是空值。
    // 每次打开窗口都通知页面重新拉取最新数据。
    let _ = app.emit("settings://refresh", ());
}

/// 主界面设置按钮入口（由注入到 DSH 页面的按钮调用）
#[tauri::command]
fn open_settings(app: AppHandle) {
    show_settings_window(&app);
}

/// 主界面右键菜单（由注入到 DSH 页面的 contextmenu 监听调用）
#[tauri::command]
fn open_context_menu(app: AppHandle) -> Result<(), String> {
    use tauri::menu::{ContextMenu, Menu as CtxMenu, MenuItem as CtxMenuItem};

    let zh = is_zh_locale();
    let (s_settings, s_restart, s_quit) = if zh {
        ("设置", "重启服务", "退出")
    } else {
        ("Settings", "Restart service", "Quit")
    };
    let settings_i = CtxMenuItem::with_id(&app, "ctx-settings", s_settings, true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let restart_i = CtxMenuItem::with_id(&app, "ctx-restart", s_restart, true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let quit_i = CtxMenuItem::with_id(&app, "ctx-quit", s_quit, true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let menu = CtxMenu::with_items(&app, &[&settings_i, &restart_i, &quit_i])
        .map_err(|e| e.to_string())?;

    if let Some(w) = app.get_webview_window("main") {
        menu.popup(w.as_ref().window()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 把阶段映射为白话文案（前端也有映射，这里用于日志）
fn phase_text(p: BootPhase) -> &'static str {
    match p {
        BootPhase::NodeCheck => "正在准备程序…",
        BootPhase::DshInstall => "正在准备首次使用…",
        BootPhase::ServiceStart => "正在启动…",
        BootPhase::Ready => "已就绪",
        BootPhase::Error => "遇到了一点问题",
    }
}

/// 系统语言是否为中文（NFR-10：托盘菜单等原生 UI 文案）
fn is_zh_locale() -> bool {
    #[cfg(windows)]
    {
        // Windows 优先读系统 UI 语言（注册表）：从 Git Bash 等环境启动时
        // LANG/LC_ALL 可能被设为 en_US，若环境变量优先会导致托盘误显示英文
        // （与网页端 navigator.language 不一致）。非中文系统再回退环境变量，
        // 保留用户用 LANG=zh_* 强制中文的能力。
        use winapi::um::winnls::GetUserDefaultUILanguage;
        let lang = unsafe { GetUserDefaultUILanguage() };
        // 0x0804 = 简体中文，0x1004 = 简体中文（新加坡），0x0404 = 繁体中文（台湾），0x0C04 = 繁体中文（香港）
        if matches!(lang, 0x0804 | 0x1004 | 0x0404 | 0x0C04) {
            return true;
        }
    }
    std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .map(|v| v.to_lowercase().starts_with("zh"))
        .unwrap_or(false)
}

fn emit_progress(app: &AppHandle, phase: BootPhase, message: &str) {
    let _ = app.emit(
        "boot://progress",
        serde_json::json!({ "phase": phase, "message": message }),
    );
}

/// 设置错误状态并广播给前端
fn fail(app: &AppHandle, state: &AppState, message: impl Into<String>, detail: &str) {
    state.set_error(message.into());
    let msg = state.error().unwrap_or_default();
    emit_progress(app, BootPhase::Error, &msg);
    update_tray_tooltip(app);
    log::error!("boot failed: {detail}");
}

/// 启动流程：校验 Node → 安装/更新 DSH → 启动服务 → 就绪
fn boot(app: AppHandle) {
    let state = app.state::<AppState>();
    // 1) Node 校验
    state.set_phase(BootPhase::NodeCheck);
    emit_progress(&app, BootPhase::NodeCheck, phase_text(BootPhase::NodeCheck));
    let resource_dir = match app.path().resource_dir() {
        Ok(d) => d,
        Err(e) => {
            fail(&app, &state, "程序文件位置异常，请重新安装。", &format!("resource_dir: {e}"));
            return;
        }
    };
    let node_path = match node::resolve_node(&resource_dir) {
        Ok(p) => p,
        Err(e) => {
            fail(&app, &state, "程序文件不完整，请重新安装。", &format!("resolve node: {e}"));
            return;
        }
    };
    match node::smoke(&node_path) {
        Ok(v) => log::info!("node ok: {v}"),
        Err(e) => {
            // FR-12 自修复：瞬时故障自动重试一次（如被杀软临时拦截）
            log::warn!("node smoke failed (retrying once): {e}");
            std::thread::sleep(Duration::from_millis(800));
            match node::smoke(&node_path) {
                Ok(v) => log::info!("node ok (after retry): {v}"),
                Err(e2) => {
                    fail(
                        &app,
                        &state,
                        "程序文件损坏，请重新安装本应用。",
                        &format!("node smoke failed twice: {e} / {e2}"),
                    );
                    return;
                }
            }
        }
    }
    state.set_node_path(node_path);

    // 2) DSH 运行时安装/更新
    state.set_phase(BootPhase::DshInstall);
    emit_progress(&app, BootPhase::DshInstall, phase_text(BootPhase::DshInstall));
    let app_data = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            fail(&app, &state, "无法确定数据目录，请重新安装。", &format!("app_data_dir: {e}"));
            return;
        }
    };
    let runtime_dir = app_data.join(RUNTIME_DIR);
    let workspace_dir = app_data.join(WORKSPACE_DIR);
    if let Err(e) = std::fs::create_dir_all(&workspace_dir) {
        log::warn!("create workspace: {e}");
    }
    state.set_runtime_dir(runtime_dir.clone());
    state.set_workspace_dir(workspace_dir);

    // 服务日志目录（FR-09：dsh stdout/stderr 落盘）
    state.supervisor.lock().unwrap().set_log_dir(app_data.join("logs"));

    let installer_js = resource_dir.join("installer").join("install-dsh.mjs");
    let baseline_dir = resource_dir.join("dsh-baseline");
    let install_out = match node::run_prepare(&state.node_path(), &installer_js, &runtime_dir, &baseline_dir) {
        Ok(o) => o,
        Err(e) => {
            fail(&app, &state, "首次准备没有成功，请重试。", &format!("installer launch: {e}"));
            return;
        }
    };
    // 解析安装器输出（末行 JSON）
    if let Some(line) = install_out.lines().last() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if json.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let msg = json
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("首次准备没有成功，请重试。")
                    .to_string();
                fail(&app, &state, msg, &format!("installer: {line}"));
                return;
            }
            log::info!("installer: {line}");
        } else {
            log::warn!("installer output not json: {line}");
        }
    }

    // 3) 启动服务
    state.set_phase(BootPhase::ServiceStart);
    emit_progress(&app, BootPhase::ServiceStart, phase_text(BootPhase::ServiceStart));
    let dsh_bin = runtime_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    if !dsh_bin.exists() {
        fail(&app, &state, "程序没有安装完整，请重试。", &format!("dsh bin missing: {}", dsh_bin.display()));
        return;
    }

    let cfg_port = state.config.lock().unwrap().get().port;
    let preferred_port = if cfg_port > 0 { cfg_port } else { 3080u16 };
    let port = {
        let mut sup = state.supervisor.lock().unwrap();
        let extra = state.dsh_extra_args.lock().unwrap().clone();
        match sup.start(&state.node_path(), &dsh_bin, &state.workspace_dir(), preferred_port, &extra) {
            Ok(p) => p,
            Err(e) => {
                drop(sup);
                fail(&app, &state, "启动没有成功，请重试。", &format!("service start: {e}"));
                return;
            }
        }
    };
    state.set_port(port);
    log::info!("service on 127.0.0.1:{port}");

    // 健康检查（首次等待 30s）
    let ready = state.supervisor.lock().unwrap().wait_ready(Duration::from_secs(30));
    if !ready {
        // 尝试重启一次（可能是端口竞态等瞬时问题）
        log::warn!("service not healthy after 30s; restarting once");
        let mut sup = state.supervisor.lock().unwrap();
        let extra = state.dsh_extra_args.lock().unwrap().clone();
        match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), preferred_port, &extra) {
            Ok(p) => {
                state.set_port(p);
                drop(sup);
                if !state.supervisor.lock().unwrap().wait_ready(Duration::from_secs(30)) {
                    fail(&app, &state, "程序没有正常启动，点这里修复。", "service unhealthy after restart");
                    return;
                }
            }
            Err(e) => {
                fail(&app, &state, "程序多次启动失败，请稍后再试。", &format!("restart: {e}"));
                return;
            }
        }
    }

    // 4) 就绪
    state.set_phase(BootPhase::Ready);
    state.clear_error();
    // 记录当前 bundle 集合（插件页据此提示"重启后生效"）
    plugins::record_restart(&state);
    let port = state.port();
    log::info!("READY http://127.0.0.1:{port}");
    update_tray_tooltip(&app);
    let _ = app.emit(
        "boot://ready",
        serde_json::json!({ "url": format!("http://127.0.0.1:{port}") }),
    );

    // 5) 后台检查/更新 DSH（不阻塞使用；结果写入状态，设置页可见，失败不打扰）
    let bg_node = state.node_path();
    let bg_installer = installer_js.clone();
    let bg_runtime = runtime_dir.clone();
    let cfg = state.config.lock().unwrap().get();
    let auto_update = cfg.auto_update_dsh;
    let registry_source = cfg.registry_source.clone();
    let bg_app = app.clone();
    std::thread::spawn(move || {
        let state = bg_app.state::<AppState>();
        let current = node::read_installed_version(&bg_runtime);
        if !auto_update {
            state.set_dsh_update(DshUpdateStatus {
                ok: true,
                update_available: false,
                current,
                latest: None,
                message: "已关闭自动更新（可在设置中开启）".into(),
            });
            log::info!("bg update: auto-update disabled by user");
            return;
        }
        // 先检查是否有新版本
        let check = match node::run_check(&bg_node, &bg_installer, &bg_runtime, &registry_source) {
            Ok(o) => o,
            Err(e) => {
                state.set_dsh_update(DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: None,
                    message: format!("后台检查更新失败：{e}"),
                });
                log::warn!("bg update check failed: {e}");
                return;
            }
        };
        let check_res = node::parse_installer_output(&check);
        if !check_res.ok {
            state.set_dsh_update(DshUpdateStatus {
                ok: false,
                update_available: false,
                current,
                latest: None,
                message: check_res.message.unwrap_or_else(|| "后台检查更新失败".into()),
            });
            return;
        }
        if check_res.action != "new-version-available" {
            state.set_dsh_update(DshUpdateStatus {
                ok: true,
                update_available: false,
                current,
                latest: check_res.version.clone(),
                message: format!("已是最新版本（{}）", check_res.version.unwrap_or_default()),
            });
            log::info!("bg update: up-to-date");
            return;
        }
        // 有新版：后台自动安装
        log::info!("bg update: new version available, installing");
        let latest = check_res.version.clone();
        state.set_dsh_update(DshUpdateStatus {
            ok: true,
            update_available: true,
            current: current.clone(),
            latest: latest.clone(),
            message: format!("发现新版本 {}，正在后台更新…", latest.unwrap_or_default()),
        });
        match node::run_update(&bg_node, &bg_installer, &bg_runtime, &registry_source) {
            Ok(o) => {
                let u = node::parse_installer_output(&o);
                let new_version = u.version.clone();
                let message = if u.ok && u.action == "updated" {
                    format!(
                        "已更新到 {}（重启服务后生效）",
                        new_version.clone().unwrap_or_default()
                    )
                } else {
                    u.message.unwrap_or_else(|| "更新结果未知，请查看日志".into())
                };
                state.set_dsh_update(DshUpdateStatus {
                    ok: u.ok,
                    update_available: false,
                    current: u.current.or(current.clone()),
                    latest: new_version,
                    message,
                });
                log::info!("bg update result: {}", o.lines().last().unwrap_or("?"));
            }
            Err(e) => {
                state.set_dsh_update(DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: None,
                    message: format!("后台更新失败：{e}"),
                });
                log::warn!("bg update failed: {e}");
            }
        }
        // 更新失败不打扰用户（下次启动会重试）；状态可在设置页查看
    });

    // 6) 后台确保默认插件（FR-18：dshmarket 市场插件，装进 dsh 网页设置。
    //    失败静默，下次启动重试；安装后需重启服务才生效——插件页会给出提示）
    let ap_node = state.node_path();
    let ap_runtime = runtime_dir;
    let ap_data = app_data;
    let ap_registry = cfg.registry_source.clone();
    std::thread::spawn(move || {
        plugins::ensure_default_plugin(&ap_node, &ap_runtime, &ap_data, &ap_registry);
    });
}

/// 前端拉取状态
#[tauri::command]
fn get_status(state: State<'_, AppState>) -> StatusSnapshot {
    let phase = state.phase();
    let dsh_version = node::read_installed_version(&state.runtime_dir());
    let node_version = node::smoke(&state.node_path()).ok();
    StatusSnapshot {
        phase,
        message: phase_text(phase).to_string(),
        error: state.error(),
        port: state.port(),
        dsh_version,
        node_version,
    }
}

/// 服务重启/修复成功后刷新主窗口：
/// - 主窗口若已加载 DSH 页面（127.0.0.1 本地服务），旧页面已随服务停止而失效，
///   直接导航到新地址（重启后端口可能已变化）；
/// - 若还在启动页，广播 boot://ready 由启动页自行跳转（保留首次引导流程）；
/// - 设置页也监听 boot://ready，借此刷新服务状态。
fn refresh_main_after_restart(app: &AppHandle, port: u16) {
    let url = format!("http://127.0.0.1:{port}");
    // 广播给所有页面（设置页刷新状态；启动页跳转）
    let _ = app.emit("boot://ready", serde_json::json!({ "url": url }));
    // 主窗口已加载 DSH 页面时直接导航（该页面没有 boot://ready 监听）
    if let Some(w) = app.get_webview_window("main") {
        let on_dsh_page = w
            .url()
            .ok()
            .map(|u| u.host_str() == Some("127.0.0.1"))
            .unwrap_or(false);
        if on_dsh_page {
            match url.parse::<tauri::Url>() {
                Ok(u) => {
                    log::info!("main window reload -> {url}");
                    let _ = w.navigate(u);
                }
                Err(e) => log::warn!("bad main window url {url}: {e}"),
            }
        }
    }
}

/// 手动重启服务（设置页/诊断页）
#[tauri::command]
fn restart_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let runtime_dir = state.runtime_dir();
    if runtime_dir.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let dsh_bin = runtime_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    let mut sup = state.supervisor.lock().unwrap();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), 3080, &extra) {
        Ok(port) => {
            state.set_port(port);
            if sup.wait_ready(Duration::from_secs(30)) {
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                plugins::record_restart(&state);
                refresh_main_after_restart(&app, port);
                Ok(())
            } else {
                state.set_error("服务没有正常启动，请稍后再试。");
                Err("服务未就绪".into())
            }
        }
        Err(e) => Err(e),
    }
}

/// 一键修复（FR-12）：强制重建 dsh 运行时（重新 prepare）+ 重启服务。
/// 用于服务反复崩溃/安装损坏的降级态修复。
#[tauri::command]
fn repair_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer_js = resource_dir.join("installer").join("install-dsh.mjs");
    let baseline_dir = resource_dir.join("dsh-baseline");
    let runtime_dir = state.runtime_dir();
    let node_path = state.node_path();

    // 1) 强制重建运行时（install-dsh.mjs prepare --force 未实现，用 update --force 重建基线等价物：
    //    直接重新 prepare 到临时目录再替换，或简化：prepare 已幂等，这里 stop 后重跑 prepare）
    state.supervisor.lock().unwrap().stop();
    let install_out = node::run_prepare(&node_path, &installer_js, &runtime_dir, &baseline_dir)
        .map_err(|e| format!("修复失败：{e}"))?;
    let line = install_out.lines().last().unwrap_or("").to_string();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
        if json.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let msg = json
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("修复没有成功，请重试。");
            return Err(msg.to_string());
        }
    }

    // 2) 重启服务
    let dsh_bin = runtime_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    if !dsh_bin.exists() {
        return Err("修复后服务仍不完整，请重新安装。".into());
    }
    let mut sup = state.supervisor.lock().unwrap();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let cfg_port = state.config.lock().unwrap().get().port;
    let preferred = if cfg_port > 0 { cfg_port } else { 3080 };
    match sup.start(&node_path, &dsh_bin, &state.workspace_dir(), preferred, &extra) {
        Ok(port) => {
            state.set_port(port);
            if sup.wait_ready(Duration::from_secs(30)) {
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                plugins::record_restart(&state);
                update_tray_tooltip(&app);
                refresh_main_after_restart(&app, port);
                Ok(())
            } else {
                state.set_error("修复后服务仍未启动，请稍后再试。");
                Err("服务未就绪".into())
            }
        }
        Err(e) => {
            state.set_error("修复后服务启动失败，请稍后再试。");
            Err(e)
        }
    }
}

/// 应用壳更新：检查并（可选）安装新版本。
/// 返回：(更新可用?, 版本号?)；安装成功后返回版本号。
#[tauri::command]
async fn check_app_update(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;

    // updater 需要 HTTP 客户端 feature；若端点未配置（占位符）则直接返回无更新
    let config = app.config();
    let endpoints = config
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("endpoints"))
        .and_then(|v| v.as_array());
    let configured = endpoints.map(|a| !a.is_empty()).unwrap_or(false);
    let placeholder = endpoints
        .map(|a| {
            a.iter()
                .any(|e| {
                    e.as_str()
                        .map(|s| s.contains("your-update-server") || s.contains("<OWNER>"))
                        .unwrap_or(false)
                })
        })
        .unwrap_or(false);
    if !configured || placeholder {
        log::info!("updater: not configured, skipping check");
        return Ok(None);
    }

    let updater = app.updater().map_err(|e| e.to_string())?;
    let check = updater.check().await.map_err(|e| e.to_string())?;
    if let Some(update) = check {
        log::info!("updater: new version {} available", update.version);
        // 下载并安装（静默；安装后重启由 updater 处理）
        let _ = update
            .download_and_install(|_chunk, _total| {}, || {})
            .await
            .map_err(|e| e.to_string())?;
        log::info!("updater: installed");
        Ok(Some(update.version.to_string()))
    } else {
        log::info!("updater: up to date");
        Ok(None)
    }
}

/// 设置页数据（白话展示）
#[derive(serde::Serialize)]
struct SettingsData {
    app_version: String,
    node_version: Option<String>,
    dsh_version: Option<String>,
    port: u16,
    /// 服务是否真正在运行（对监听端口做实时健康检查，而非仅看记录的端口）
    service_running: bool,
    /// 当前启动阶段（前端据此显示"正在启动…/启动失败"等）
    phase: BootPhase,
    error: Option<String>,
    workspace_dir: String,
    log_file: String,
    autostart_enabled: bool,
}

#[tauri::command]
fn get_settings(app: AppHandle, state: State<'_, AppState>) -> Result<SettingsData, String> {
    use tauri_plugin_autostart::ManagerExt;

    let pkg = app.package_info();
    let autostart = app
        .autolaunch()
        .is_enabled()
        .map_err(|e| format!("读取开机启动设置失败: {e}"))?;

    let port = state.port();
    let service_running = port > 0 && state.supervisor.lock().unwrap().health_check();

    Ok(SettingsData {
        app_version: pkg.version.to_string(),
        node_version: node::smoke(&state.node_path()).ok(),
        dsh_version: node::read_installed_version(&state.runtime_dir()),
        port,
        service_running,
        phase: state.phase(),
        error: state.error(),
        workspace_dir: state.workspace_dir().display().to_string(),
        log_file: state.log_file().display().to_string(),
        autostart_enabled: autostart,
    })
}

/// 设置开机自启
#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    if enabled {
        app.autolaunch()
            .enable()
            .map_err(|e| format!("开启开机启动失败: {e}"))?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|e| format!("关闭开机启动失败: {e}"))?;
    }
    log::info!("autostart set to {enabled}");
    Ok(())
}

/// 打开日志目录（系统文件管理器）
#[tauri::command]
fn open_log_dir(state: State<'_, AppState>) -> Result<(), String> {
    let log_file = state.log_file();
    let dir = log_file
        .parent()
        .ok_or("无法确定日志位置")?;
    open::that(dir).map_err(|e| format!("打开日志目录失败: {e}"))?;
    Ok(())
}

/// 打开工作区目录（用户数据）
#[tauri::command]
fn open_workspace_dir(state: State<'_, AppState>) -> Result<(), String> {
    let ws = state.workspace_dir();
    if ws.as_os_str().is_empty() {
        return Err("工作区尚未初始化".into());
    }
    open::that(&ws).map_err(|e| format!("打开工作区失败: {e}"))?;
    Ok(())
}

/// 读取用户配置
#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Result<config::AppConfig, String> {
    Ok(state.config.lock().unwrap().get())
}

/// 保存用户配置
#[tauri::command]
fn set_config(state: State<'_, AppState>, config: config::AppConfig) -> Result<(), String> {
    state.config.lock().unwrap().set(config)
}

/// 读取最近一次 DSH 更新状态（后台自动更新 / 手动检查的结果，无网络开销）
#[tauri::command]
fn get_dsh_update_status(state: State<'_, AppState>) -> Option<DshUpdateStatus> {
    state.dsh_update()
}

/// 手动检查 DSH 更新（联网查询最新版本；较慢，放线程池执行）
#[tauri::command]
async fn check_dsh_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DshUpdateStatus, String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer = resource_dir.join("installer").join("install-dsh.mjs");
    let registry_source = state.config.lock().unwrap().get().registry_source;
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let status = match node::run_check(&node, &installer, &runtime, &registry_source) {
            Ok(out) => {
                let r = node::parse_installer_output(&out);
                if !r.ok {
                    DshUpdateStatus {
                        ok: false,
                        update_available: false,
                        current,
                        latest: None,
                        message: r.message.unwrap_or_else(|| "暂时无法检查更新".into()),
                    }
                } else if r.action == "new-version-available" {
                    DshUpdateStatus {
                        ok: true,
                        update_available: true,
                        current,
                        latest: r.version.clone(),
                        message: format!("发现新版本 {}", r.version.unwrap_or_default()),
                    }
                } else {
                    DshUpdateStatus {
                        ok: true,
                        update_available: false,
                        current,
                        latest: r.version.clone(),
                        message: format!("已是最新版本（{}）", r.version.unwrap_or_default()),
                    }
                }
            }
            Err(e) => DshUpdateStatus {
                ok: false,
                update_available: false,
                current,
                latest: None,
                message: format!("检查更新失败：{e}"),
            },
        };
        app2.state::<AppState>().set_dsh_update(status.clone());
        status
    })
    .await
    .map_err(|e| format!("检查更新任务异常: {e}"))?)
}

/// 手动更新 DSH 到最新版（联网安装，可能耗时数分钟；完成后需重启服务生效）
#[tauri::command]
async fn update_dsh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DshUpdateStatus, String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer = resource_dir.join("installer").join("install-dsh.mjs");
    let registry_source = state.config.lock().unwrap().get().registry_source;
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let status = match node::run_update(&node, &installer, &runtime, &registry_source) {
            Ok(out) => {
                let u = node::parse_installer_output(&out);
                if u.ok && u.action == "updated" {
                    DshUpdateStatus {
                        ok: true,
                        update_available: false,
                        current: u.current.or(current),
                        latest: u.version.clone(),
                        message: format!("已更新到 {}（重启服务后生效）", u.version.unwrap_or_default()),
                    }
                } else if u.ok {
                    DshUpdateStatus {
                        ok: true,
                        update_available: false,
                        current,
                        latest: u.version.clone(),
                        message: format!("已是最新版本（{}）", u.version.unwrap_or_default()),
                    }
                } else {
                    DshUpdateStatus {
                        ok: false,
                        update_available: false,
                        current,
                        latest: u.version,
                        message: u.message.unwrap_or_else(|| "更新失败".into()),
                    }
                }
            }
            Err(e) => DshUpdateStatus {
                ok: false,
                update_available: false,
                current,
                latest: None,
                message: format!("更新失败：{e}"),
            },
        };
        app2.state::<AppState>().set_dsh_update(status.clone());
        status
    })
    .await
    .map_err(|e| format!("更新任务异常: {e}"))?)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let logger = Arc::new(logging::FileLogger::new());
    // log::set_boxed_logger 需要 Box::leak 提供 'static
    let _ = log::set_logger(Box::leak(Box::new(logging::TauriLogBridge(logger.clone()))));
    log::set_max_level(log::LevelFilter::Info);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二实例：聚焦已有主窗口
            show_main_window(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(AppState {
            node_path: std::sync::Mutex::new(PathBuf::new()),
            app_data_dir: std::sync::Mutex::new(PathBuf::new()),
            runtime_dir: std::sync::Mutex::new(PathBuf::new()),
            workspace_dir: std::sync::Mutex::new(PathBuf::new()),
            log_file: std::sync::Mutex::new(PathBuf::new()),
            port: std::sync::atomic::AtomicU16::new(0),
            phase: std::sync::atomic::AtomicU8::new(0),
            error_message: std::sync::Mutex::new(None),
            supervisor: std::sync::Mutex::new(Supervisor::new()),
            config: std::sync::Mutex::new(config::ConfigStore::new(std::path::Path::new(""))),
            dsh_extra_args: std::sync::Mutex::new(Vec::new()),
            dsh_update: std::sync::Mutex::new(None),
        })
        .setup(move |app| {
            // 配置存储（appData/config.json）+ 应用数据目录（插件快照等）
            if let Ok(data) = app.path().app_data_dir() {
                let store = config::ConfigStore::new(&data);
                let state = app.state::<AppState>();
                *state.config.lock().unwrap() = store;
                state.set_app_data_dir(data);
            }
            // 日志文件
            if let Ok(data) = app.path().app_data_dir() {
                let log_path = data.join(LOG_FILE);
                if let Err(e) = logger.init(&log_path) {
                    eprintln!("log init: {e}");
                }
                let state = app.state::<AppState>();
                state.set_log_file(log_path);
                log::info!("dsh-desktop starting; platform={}", std::env::consts::OS);

                // 高级入口（FR-15）：解析 `--dsh-args "<参数>"` 透传给 dsh
                // 支持两种形式：`--dsh-args --patch a.yml` 或 `--dsh-args="--patch a.yml"`
                let mut extra: Vec<String> = Vec::new();
                let mut iter = std::env::args().skip(1);
                while let Some(a) = iter.next() {
                    if let Some(v) = a.strip_prefix("--dsh-args=") {
                        for part in v.split_whitespace() {
                            extra.push(part.to_string());
                        }
                    } else if a == "--dsh-args" {
                        if let Some(v) = iter.next() {
                            for part in v.split_whitespace() {
                                extra.push(part.to_string());
                            }
                        }
                    }
                }
                if !extra.is_empty() {
                    *state.dsh_extra_args.lock().unwrap() = extra.clone();
                    log::info!("dsh extra args: {:?}", extra);
                }
            }

            // 托盘（NFR-10：按系统语言显示菜单文案）
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::TrayIconBuilder;
            let zh = is_zh_locale();
            let (s_show, s_settings, s_restart, s_quit) = if zh {
                ("打开界面", "设置", "重启服务", "退出")
            } else {
                ("Open", "Settings", "Restart service", "Quit")
            };
            let show_i = MenuItem::with_id(app, "show", s_show, true, None::<&str>)?;
            let settings_i = MenuItem::with_id(app, "settings", s_settings, true, None::<&str>)?;
            let restart_i = MenuItem::with_id(app, "restart", s_restart, true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", s_quit, true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &settings_i, &restart_i, &quit_i])?;
            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                // 左键点击托盘图标 → 打开/聚焦主窗口；右键 → 弹出菜单
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        show_main_window(app);
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        show_main_window(app);
                    }
                    "settings" => {
                        show_settings_window(app);
                    }
                    "restart" => {
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            let state = handle.state::<AppState>();
                            let runtime_dir = state.runtime_dir();
                            if runtime_dir.as_os_str().is_empty() {
                                return;
                            }
                            let dsh_bin = runtime_dir
                                .join("node_modules")
                                .join("@deepseek-ai")
                                .join("dsh")
                                .join("lib")
                                .join("bin.js");
                            let mut sup = state.supervisor.lock().unwrap();
                            let extra = state.dsh_extra_args.lock().unwrap().clone();
                            match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), 3080, &extra) {
                                Ok(port) => {
                                    state.set_port(port);
                                    if sup.wait_ready(Duration::from_secs(30)) {
                                        state.set_phase(BootPhase::Ready);
                                        state.clear_error();
                                        log::info!("tray restart -> 127.0.0.1:{port}");
                                        // 主窗口若已加载 DSH 页面，刷新到新地址
                                        refresh_main_after_restart(&handle, port);
                                    } else {
                                        log::warn!("tray restart: service not healthy");
                                    }
                                }
                                Err(e) => log::error!("tray restart failed: {e}"),
                            }
                        });
                    }
                    "quit" => {
                        // 先停服务（杀进程树），再退出；避免残留 dsh/node 进程
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            let state = handle.state::<AppState>();
                            state.supervisor.lock().unwrap().stop();
                            std::thread::sleep(Duration::from_millis(300));
                            handle.exit(0);
                        });
                    }
                    _ => {}
                })
                .build(app)?;

            // 右键菜单（主界面设置入口）：DSH 页面上右键弹出原生菜单
            // 设置 / 重启服务 / 退出——菜单在 open_context_menu 时按需构建
            // （无常驻菜单栏，符合"右击"入口）

            // 启动 boot（后台线程）
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                boot(app_handle);
            });
            Ok(())
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "ctx-settings" => {
                show_settings_window(app);
            }
            "ctx-restart" => {
                let handle = app.clone();
                std::thread::spawn(move || {
                    let state = handle.state::<AppState>();
                    let runtime_dir = state.runtime_dir();
                    if runtime_dir.as_os_str().is_empty() {
                        return;
                    }
                    let dsh_bin = runtime_dir
                        .join("node_modules")
                        .join("@deepseek-ai")
                        .join("dsh")
                        .join("lib")
                        .join("bin.js");
                    let mut sup = state.supervisor.lock().unwrap();
                    let extra = state.dsh_extra_args.lock().unwrap().clone();
                    match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), 3080, &extra) {
                        Ok(port) => {
                            state.set_port(port);
                            if sup.wait_ready(Duration::from_secs(30)) {
                                state.set_phase(BootPhase::Ready);
                                state.clear_error();
                                log::info!("ctx restart -> 127.0.0.1:{port}");
                                // 主窗口若已加载 DSH 页面，刷新到新地址
                                refresh_main_after_restart(&handle, port);
                            } else {
                                log::warn!("ctx restart: service not healthy");
                            }
                        }
                        Err(e) => log::error!("ctx restart failed: {e}"),
                    }
                });
            }
            "ctx-quit" => {
                let handle = app.clone();
                std::thread::spawn(move || {
                    let state = handle.state::<AppState>();
                    state.supervisor.lock().unwrap().stop();
                    std::thread::sleep(Duration::from_millis(300));
                    handle.exit(0);
                });
            }
            _ => {}
        })
        .on_page_load(|webview, payload| {
            // 主界面右键入口：DSH 页面加载完成后挂接 contextmenu 监听，
            // 右键弹出原生菜单（设置/重启服务/退出）——无任何常驻 UI
            if let tauri::webview::PageLoadEvent::Finished = payload.event() {
                if payload.url().scheme() == "http" && webview.label() == "main" {
                    let script = r#"
(function () {
  if (window.__dshCtxInstalled) return;
  window.__dshCtxInstalled = true;
  document.addEventListener('contextmenu', function (e) {
    e.preventDefault();
    try {
      if (window.__TAURI__ && window.__TAURI__.core) {
        window.__TAURI__.core.invoke('open_context_menu');
      }
    } catch (err) { console.error('context menu failed', err); }
  }, true);
})();
"#;
                    let _ = webview.eval(script);
                }
            }
        })
        .on_window_event(|window, event| {
            // 关闭窗口 → 隐藏（不销毁、不退出），以便再次从托盘/菜单打开
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                match window.label() {
                    "main" => {
                        let _ = window.hide();
                        api.prevent_close();
                    }
                    "settings" => {
                        let _ = window.hide();
                        api.prevent_close();
                    }
                    _ => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            restart_service,
            repair_service,
            check_app_update,
            get_settings,
            set_autostart,
            open_log_dir,
            open_workspace_dir,
            get_config,
            set_config,
            get_dsh_update_status,
            check_dsh_update,
            update_dsh,
            open_settings,
            open_context_menu,
            plugins::plugins_list,
            plugins::plugins_add,
            plugins::plugins_remove,
            plugins::plugins_set_enabled,
            plugins::plugins_marketplace
        ])
        .run(tauri::generate_context!())
        .expect("error while running dsh-desktop");
}
