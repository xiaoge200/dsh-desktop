mod config;
mod logging;
mod node;
mod state;
mod supervisor;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use state::{AppState, BootPhase, StatusSnapshot};
use supervisor::Supervisor;
use tauri::{AppHandle, Emitter, Manager, State};

/// 应用数据目录下的子目录
const RUNTIME_DIR: &str = "dsh-runtime";
const WORKSPACE_DIR: &str = "workspace";
const LOG_FILE: &str = "dsh-desktop.log";

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
            fail(&app, &state, "程序文件损坏，请重新安装。", &format!("node smoke: {e}"));
            return;
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
        match sup.start(&state.node_path(), &dsh_bin, &state.workspace_dir(), preferred_port) {
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
        match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), preferred_port) {
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
    let port = state.port();
    log::info!("READY http://127.0.0.1:{port}");
    let _ = app.emit(
        "boot://ready",
        serde_json::json!({ "url": format!("http://127.0.0.1:{port}") }),
    );

    // 5) 后台检查/更新 DSH（不阻塞使用；失败静默，用户无感）
    let bg_node = state.node_path();
    let bg_installer = installer_js;
    let bg_runtime = runtime_dir;
    let auto_update = state.config.lock().unwrap().get().auto_update_dsh;
    std::thread::spawn(move || {
        if !auto_update {
            log::info!("bg update: auto-update disabled by user");
            return;
        }
        // 先检查是否有新版本
        let check = match node::run_check(&bg_node, &bg_installer, &bg_runtime) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("bg update check failed: {e}");
                return;
            }
        };
        let should_update = check
            .lines()
            .last()
            .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .and_then(|j| j.get("action").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .map(|a| a == "new-version-available")
            .unwrap_or(false);
        if !should_update {
            log::info!("bg update: up-to-date");
            return;
        }
        // 有新版：后台自动安装（提示式改自动：仅失败时才提示）
        log::info!("bg update: new version available, installing");
        match node::run_update(&bg_node, &bg_installer, &bg_runtime) {
            Ok(o) => log::info!("bg update result: {}", o.lines().last().unwrap_or("?")),
            Err(e) => log::warn!("bg update failed: {e}"),
        }
        // 更新失败不打扰用户（下次启动会重试）；日志可查
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
    match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), 3080) {
        Ok(port) => {
            state.set_port(port);
            if sup.wait_ready(Duration::from_secs(30)) {
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                let _ = app.emit(
                    "boot://ready",
                    serde_json::json!({ "url": format!("http://127.0.0.1:{port}") }),
                );
                Ok(())
            } else {
                state.set_error("服务没有正常启动，请稍后再试。");
                Err("服务未就绪".into())
            }
        }
        Err(e) => Err(e),
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
                .any(|e| e.as_str().map(|s| s.contains("your-update-server")).unwrap_or(false))
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

    Ok(SettingsData {
        app_version: pkg.version.to_string(),
        node_version: node::smoke(&state.node_path()).ok(),
        dsh_version: node::read_installed_version(&state.runtime_dir()),
        port: state.port(),
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let logger = Arc::new(logging::FileLogger::new());
    // log::set_boxed_logger 需要 Box::leak 提供 'static
    let _ = log::set_logger(Box::leak(Box::new(logging::TauriLogBridge(logger.clone()))));
    log::set_max_level(log::LevelFilter::Info);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二实例：聚焦已有主窗口
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(AppState {
            node_path: std::sync::Mutex::new(PathBuf::new()),
            runtime_dir: std::sync::Mutex::new(PathBuf::new()),
            workspace_dir: std::sync::Mutex::new(PathBuf::new()),
            log_file: std::sync::Mutex::new(PathBuf::new()),
            port: std::sync::atomic::AtomicU16::new(0),
            phase: std::sync::atomic::AtomicU8::new(0),
            error_message: std::sync::Mutex::new(None),
            supervisor: std::sync::Mutex::new(Supervisor::new()),
            config: std::sync::Mutex::new(config::ConfigStore::new(std::path::Path::new(""))),
        })
        .setup(move |app| {
            // 配置存储（appData/config.json）
            if let Ok(data) = app.path().app_data_dir() {
                let store = config::ConfigStore::new(&data);
                let state = app.state::<AppState>();
                *state.config.lock().unwrap() = store;
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
            }

            // 托盘
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::TrayIconBuilder;
            let show_i = MenuItem::with_id(app, "show", "打开界面", true, None::<&str>)?;
            let settings_i = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
            let restart_i = MenuItem::with_id(app, "restart", "重启服务", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &settings_i, &restart_i, &quit_i])?;
            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                    "settings" => {
                        if let Some(w) = app.get_webview_window("settings") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
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
                            match sup.restart(&state.node_path(), &dsh_bin, &state.workspace_dir(), 3080) {
                                Ok(port) => {
                                    state.set_port(port);
                                    log::info!("tray restart -> 127.0.0.1:{port}");
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

            // 启动 boot（后台线程）
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                boot(app_handle);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口 → 最小化到托盘（不退出）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            restart_service,
            check_app_update,
            get_settings,
            set_autostart,
            open_log_dir,
            open_workspace_dir,
            get_config,
            set_config
        ])
        .run(tauri::generate_context!())
        .expect("error while running dsh-desktop");
}
