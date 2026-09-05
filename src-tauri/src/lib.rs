mod app_update;
mod config;
mod dsh_update;
mod logging;
mod node;
mod plugins;
mod service;
mod serviceout;
mod settings;
mod state;
mod supervisor;
#[cfg(windows)]
mod winproc;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use app_update::{check_app_update, get_app_update_status, update_app};
use dsh_update::{check_dsh_update, get_dsh_update_status, spawn_bg_dsh_update, update_dsh};
use plugins::plugin_market::plugins_marketplace;
use service::{
    emit_error_options, menu_restart, preferred_port, repair_service, restart_service,
    service_url, spawn_service_watch, start_service_with_heal, wait_service_url,
};
use settings::{
    get_config, get_service_state, get_settings, open_log_dir, open_workspace_dir, set_autostart,
    set_config,
};
use state::{AppState, BootPhase, StatusSnapshot};
use supervisor::Supervisor;
use tauri::{AppHandle, Emitter, Manager, State};

const RUNTIME_DIR: &str = "dsh-runtime";
const WORKSPACE_DIR: &str = "workspace";
const LOG_FILE: &str = "dsh-desktop.log";

fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub(crate) fn update_tray_tooltip(app: &AppHandle) {
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

fn show_settings_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    } else {
        log::warn!("settings window not found; creating");
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
    let _ = app.emit("settings://refresh", ());
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    show_settings_window(&app);
}


#[derive(serde::Deserialize)]
struct NotifyReq {
    title: String,
    body: String,
}

fn show_notify(app: &AppHandle, req: NotifyReq) {
    log::info!("notify: {} | {}", req.title, req.body);
    use tauri_plugin_notification::NotificationExt;
    match app
        .notification()
        .builder()
        .title(&req.title)
        .body(&req.body)
        .show()
    {
        Ok(_) => log::info!("notify ok"),
        Err(e) => log::warn!("notify failed: {e}"),
    }
}

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

fn phase_text(p: BootPhase) -> &'static str {
    match p {
        BootPhase::NodeCheck => "正在准备程序…",
        BootPhase::DshInstall => "正在准备首次使用…",
        BootPhase::ServiceStart => "正在启动…",
        BootPhase::Ready => "已就绪",
        BootPhase::Error => "遇到了一点问题",
    }
}

pub(crate) fn is_zh_locale() -> bool {
    #[cfg(windows)]
    {
        use winapi::um::winnls::GetUserDefaultUILanguage;
        let lang = unsafe { GetUserDefaultUILanguage() };
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

pub(crate) fn emit_progress(app: &AppHandle, phase: BootPhase, message: &str) {
    let _ = app.emit(
        "boot://progress",
        serde_json::json!({ "phase": phase, "message": message }),
    );
}

fn fail(app: &AppHandle, state: &AppState, message: impl Into<String>, detail: &str) {
    state.set_error(message.into());
    let msg = state.error().unwrap_or_default();
    emit_progress(app, BootPhase::Error, &msg);
    update_tray_tooltip(app);
    log::error!("boot failed: {detail}");
}

fn boot(app: AppHandle) {
    let state = app.state::<AppState>();
    if let Some(w) = app.get_webview_window("main") {
        if let Ok(u) = w.url() {
            state.set_boot_page_url(u.to_string());
        }
    }
    state.clear_recovery();
    state.set_pending_auth_url(None);
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

    state.set_phase(BootPhase::ServiceStart);
    emit_progress(&app, BootPhase::ServiceStart, phase_text(BootPhase::ServiceStart));
    let dsh_bin = node::dsh_bin(&runtime_dir);
    if !dsh_bin.exists() {
        fail(&app, &state, "程序没有安装完整，请重试。", &format!("dsh bin missing: {}", dsh_bin.display()));
        return;
    }

    let preferred_port = preferred_port(&state);
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let started = start_service_with_heal(
        &app,
        &state,
        &state.node_path(),
        &dsh_bin,
        &state.workspace_dir(),
        preferred_port,
        &extra,
        true,
    );
    match started {
        Ok(()) => {}
        Err(issue) if issue.spawn_error => {
            let msg = if issue.detail.contains("多次启动失败") {
                "程序多次启动失败，请稍后再试。"
            } else {
                "启动没有成功，请重试。"
            };
            fail(&app, &state, msg, &format!("service start: {}", issue.detail));
            return;
        }
        Err(issue) => {
            if issue.kind == serviceout::FailureKind::PluginTree {
                fail(
                    &app,
                    &state,
                    "部分已安装插件与当前 DSH 版本不兼容，服务无法启动。",
                    &issue.detail,
                );
                emit_error_options(&app, &state, &issue);
                return;
            }
            log::warn!("service not healthy after start; restarting once");
            match start_service_with_heal(
                &app,
                &state,
                &state.node_path(),
                &dsh_bin,
                &state.workspace_dir(),
                preferred_port,
                &extra,
                false,
            ) {
                Ok(()) => {}
                Err(issue2) => {
                    let msg = if issue2.spawn_error {
                        if issue2.detail.contains("多次启动失败") {
                            "程序多次启动失败，请稍后再试。"
                        } else {
                            "程序没有正常启动，点这里修复。"
                        }
                    } else {
                        "程序没有正常启动，点这里修复。"
                    };
                    fail(&app, &state, msg, &issue2.detail);
                    if !issue2.spawn_error {
                        emit_error_options(&app, &state, &issue2);
                    }
                    return;
                }
            }
        }
    }

    wait_service_url(&state, Duration::from_secs(2));
    state.set_phase(BootPhase::Ready);
    state.clear_error();
    plugins::record_restart(&state);
    let url = service_url(&state);
    state.set_pending_auth_url(Some(url.clone()));
    log::info!("READY {url}");
    update_tray_tooltip(&app);
    let _ = app.emit("boot://ready", serde_json::json!({ "url": url }));
    spawn_service_watch(&app);

    spawn_bg_dsh_update(&app, &installer_js);

    spawn_ensure_default_plugin(&app);
}

fn spawn_ensure_default_plugin(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let node = state.node_path();
        let runtime = state.runtime_dir();
        let app_data = state.app_data_dir();
        let registry = state.config.lock().unwrap().get().registry_source;
        plugins::ensure_default_plugin(&node, &runtime, &app_data, &registry);
    });
}

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
        service_url: if phase == BootPhase::Ready {
            let u = service_url(&state);
            if u.contains("?token=") {
                state.set_pending_auth_url(Some(u.clone()));
            }
            Some(u)
        } else {
            None
        },
        recovery: state.last_recovery(),
        dsh_version,
        node_version,
    }
}

fn parse_dsh_extra_args() -> Vec<String> {
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
    extra
}

fn quit_app(app: &AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        state.supervisor.lock().unwrap().stop();
        std::thread::sleep(Duration::from_millis(300));
        handle.exit(0);
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
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
                menu_restart(app, "tray");
            }
            "quit" => {
                quit_app(app);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "ctx-settings" => {
            show_settings_window(app);
        }
        "ctx-restart" => {
            menu_restart(app, "ctx");
        }
        "ctx-quit" => {
            quit_app(app);
        }
        _ => {}
    }
}

fn handle_page_load(
    webview: &tauri::Webview<tauri::Wry>,
    payload: &tauri::webview::PageLoadPayload<'_>,
) {
    if let tauri::webview::PageLoadEvent::Finished = payload.event() {
        if payload.url().scheme() == "http" && webview.label() == "main" {
            if let Err(e) = webview.eval(include_str!("../assets/ctx-menu.js")) {
                log::warn!("ctx script eval failed: {e}");
            }
            if payload.url().host_str() == Some("127.0.0.1") {
                if let Err(e) = webview.eval(include_str!("../assets/notify-shim.js")) {
                    log::warn!("notify shim eval failed: {e}");
                }
                if let Err(e) = webview.eval(include_str!("../assets/restart-shim.js")) {
                    log::warn!("restart shim eval failed: {e}");
                }
            }
            if payload.url().query().is_none() {
                let app = webview.app_handle();
                let state = app.state::<AppState>();
                if let Some(pending) = state.pending_auth_url() {
                    if pending.contains("?token=") {
                        state.set_pending_auth_url(None);
                        let js = format!(
                            "window.location.href = {};",
                            serde_json::to_string(&pending).unwrap_or_default()
                        );
                        log::info!("two-hop auth exchange on service origin");
                        let _ = webview.eval(&js);
                    }
                }
            }
        }
    }
}

fn handle_window_event(window: &tauri::Window<tauri::Wry>, event: &tauri::WindowEvent) {
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
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let logger = Arc::new(logging::FileLogger::new());
    let _ = log::set_logger(Box::leak(Box::new(logging::TauriLogBridge(logger.clone()))));
    log::set_max_level(log::LevelFilter::Info);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
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
            app_update: std::sync::Mutex::new(None),
            boot_page_url: std::sync::Mutex::new(None),
            service_watch_active: std::sync::atomic::AtomicBool::new(false),
            last_recovery: std::sync::Mutex::new(None),
            pending_auth_url: std::sync::Mutex::new(None),
        })
        .setup(move |app| {
            {
                use tauri::Listener;
                let h1 = app.handle().clone();
                let h2 = h1.clone();
                h1.listen("dsh-notify-request", move |ev| {
                    if let Ok(req) = serde_json::from_str::<NotifyReq>(ev.payload()) {
                        show_notify(&h2, req);
                    }
                });
            }
            if let Ok(data) = app.path().app_data_dir() {
                let store = config::ConfigStore::new(&data);
                let state = app.state::<AppState>();
                *state.config.lock().unwrap() = store;
                state.set_app_data_dir(data);
            }
            if let Ok(data) = app.path().app_data_dir() {
                let log_path = data.join(LOG_FILE);
                if let Err(e) = logger.init(&log_path) {
                    eprintln!("log init: {e}");
                }
                let state = app.state::<AppState>();
                state.set_log_file(log_path);
                log::info!("dsh-desktop starting; platform={}", std::env::consts::OS);

                let extra = parse_dsh_extra_args();
                if !extra.is_empty() {
                    *state.dsh_extra_args.lock().unwrap() = extra.clone();
                    log::info!("dsh extra args: {:?}", extra);
                }
            }

            build_tray(app.handle())?;

            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                boot(app_handle);
            });
            Ok(())
        })
        .on_menu_event(handle_menu_event)
        .on_page_load(handle_page_load)
        .on_window_event(handle_window_event)
        .invoke_handler(tauri::generate_handler![
            get_status,
            restart_service,
            repair_service,
            get_settings,
            get_service_state,
            set_autostart,
            open_log_dir,
            open_workspace_dir,
            get_config,
            set_config,
            get_dsh_update_status,
            check_dsh_update,
            update_dsh,
            get_app_update_status,
            check_app_update,
            update_app,
            open_settings,
            open_context_menu,
            plugins::plugins_list,
            plugins::plugins_add,
            plugins::plugins_remove,
            plugins::plugins_remove_incompatible,
            plugins::plugins_set_enabled,
            plugins_marketplace
        ])
        .run(tauri::generate_context!())
        .expect("error while running dsh-desktop");
}
