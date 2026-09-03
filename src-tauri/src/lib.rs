mod config;
mod logging;
mod node;
mod plugins;
mod serviceout;
mod state;
mod supervisor;
#[cfg(windows)]
mod winproc;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use state::{AppState, BootPhase, DshUpdateStatus, RecoveryInfo, StatusSnapshot};
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

fn is_zh_locale() -> bool {
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

fn emit_progress(app: &AppHandle, phase: BootPhase, message: &str) {
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

    let bg_node = state.node_path();
    let bg_installer = installer_js.clone();
    let bg_runtime = runtime_dir.clone();
    let bg_workspace = state.workspace_dir();
    let bg_extra = state.dsh_extra_args.lock().unwrap().clone();
    let cfg = state.config.lock().unwrap().get();
    let auto_update = cfg.auto_update_dsh;
    let pre_release = cfg.pre_release;
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
                ..Default::default()
            });
            log::info!("bg update: auto-update disabled by user");
            return;
        }
        std::thread::sleep(Duration::from_secs(30));
        let check = match node::run_check(&bg_node, &bg_installer, &bg_runtime, &registry_source, pre_release) {
            Ok(o) => o,
            Err(e) => {
                state.set_dsh_update(DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: None,
                    message: format!("后台检查更新失败：{e}"),
                    ..Default::default()
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
                ..Default::default()
            });
            return;
        }
        let has_update =
            check_res.action == "new-version-available" || check_res.action == "prerelease-available";
        if !has_update {
            state.set_dsh_update(DshUpdateStatus {
                ok: true,
                update_available: false,
                current,
                latest: check_res.version.clone(),
                prerelease: check_res.prerelease.clone(),
                pre_available: check_res.pre_available,
                message: up_to_date_message(&check_res.version),
            });
            log::info!("bg update: up-to-date");
            return;
        }
        log::info!("bg update: new version available, installing");
        let latest = check_res.version.clone();
        state.set_dsh_update(DshUpdateStatus {
            ok: true,
            update_available: true,
            current: current.clone(),
            latest: latest.clone(),
            prerelease: check_res.prerelease.clone(),
            pre_available: check_res.pre_available,
            message: format!("发现新版本 {}，正在后台更新…", latest.unwrap_or_default()),
        });
        let update_out = match node::run_update(&bg_node, &bg_installer, &bg_runtime, &registry_source, pre_release) {
            Ok(o) => o,
            Err(e) => {
                state.set_dsh_update(DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: None,
                    message: format!("后台更新失败：{e}"),
                    ..Default::default()
                });
                log::warn!("bg update failed: {e}");
                return;
            }
        };
        let u = node::parse_installer_output(&update_out);
        if !u.ok || u.action != "downloaded" {
            let message = u.message.unwrap_or_else(|| "更新结果未知，请查看日志".into());
            state.set_dsh_update(DshUpdateStatus {
                ok: u.ok,
                update_available: false,
                current: current.clone(),
                latest: u.version.clone(),
                prerelease: u.prerelease.clone(),
                pre_available: u.pre_available,
                message,
            });
            log::warn!("bg update download failed: {}", update_out.lines().last().unwrap_or("?"));
            return;
        }
        log::info!("bg update downloaded {} to staging, swapping", u.version.clone().unwrap_or_default());
        state.supervisor.lock().unwrap().stop();
        let staging = match &u.staging {
            Some(s) => std::path::PathBuf::from(s),
            None => {
                state.set_dsh_update(DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: u.version.clone(),
                    message: "更新缺少暂存目录，已保留当前版本。".into(),
                    ..Default::default()
                });
                restart_dsh_service(&bg_app, &state, &bg_node, &bg_runtime, &bg_workspace, &bg_extra);
                return;
            }
        };
        let swap_out = node::run_swap(&bg_node, &bg_installer, &bg_runtime, &staging);
        let sw = match &swap_out {
            Ok(o) => node::parse_installer_output(o),
            Err(e) => {
                state.set_dsh_update(DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: u.version.clone(),
                    message: format!("后台替换失败：{e}"),
                    ..Default::default()
                });
                log::warn!("bg update swap failed: {e}");
                restart_dsh_service(&bg_app, &state, &bg_node, &bg_runtime, &bg_workspace, &bg_extra);
                return;
            }
        };
        let new_version = sw.version.clone();
        let message = if sw.ok && sw.action == "updated" {
            format!(
                "已更新到 {}（重启服务后生效）",
                new_version.clone().unwrap_or_default()
            )
        } else {
            sw.message.unwrap_or_else(|| "更新结果未知，请查看日志".into())
        };
        state.set_dsh_update(DshUpdateStatus {
            ok: sw.ok,
            update_available: false,
            current: sw.current.or(current.clone()),
            latest: new_version,
            prerelease: sw.prerelease.clone(),
            pre_available: sw.pre_available,
            message,
        });
        log::info!("bg update result: {}", swap_out.as_deref().unwrap_or("?").lines().last().unwrap_or("?"));
        restart_dsh_service(&bg_app, &state, &bg_node, &bg_runtime, &bg_workspace, &bg_extra);
    });

    let ap_node = state.node_path();
    let ap_runtime = runtime_dir;
    let ap_data = app_data;
    let ap_registry = cfg.registry_source.clone();
    std::thread::spawn(move || {
        plugins::ensure_default_plugin(&ap_node, &ap_runtime, &ap_data, &ap_registry);
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

fn up_to_date_message(version: &Option<String>) -> String {
    match version {
        Some(v) if !v.trim().is_empty() => format!("已是最新版本（{v}）"),
        _ => "已是最新版本".to_string(),
    }
}

fn refresh_main_after_restart(app: &AppHandle, state: &AppState) {
    let url = service_url(state);
    state.set_pending_auth_url(Some(url.clone()));
    let _ = app.emit("boot://ready", serde_json::json!({ "url": url }));
    if let Some(w) = app.get_webview_window("main") {
        let on_dsh_page = w
            .url()
            .ok()
            .map(|u| u.host_str() == Some("127.0.0.1"))
            .unwrap_or(false);
        if on_dsh_page {
            let target = bare_service_url(&url);
            match target.parse::<tauri::Url>() {
                Ok(u) => {
                    log::info!("main window reload -> {target}");
                    let _ = w.navigate(u);
                }
                Err(e) => log::warn!("bad main window url {target}: {e}"),
            }
        }
    }
    spawn_service_watch(app);
}

fn bare_service_url(url: &str) -> String {
    match url.parse::<tauri::Url>() {
        Ok(mut u) => {
            u.set_query(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

fn menu_restart(app: &AppHandle, tag: &'static str) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
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
        let extra = state.dsh_extra_args.lock().unwrap().clone();
        let preferred = preferred_port(&state);
        match start_service_with_heal(
            &app,
            &state,
            &state.node_path(),
            &dsh_bin,
            &state.workspace_dir(),
            preferred,
            &extra,
            false,
        ) {
            Ok(()) => {
                wait_service_url(&state, Duration::from_secs(2));
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                log::info!("{tag} restart ready: {}", service_url(&state));
                refresh_main_after_restart(&app, &state);
            }
            Err(issue) => {
                log::warn!(
                    "{tag} restart failed ({}): {}",
                    issue.kind.kind_str(),
                    issue.detail
                );
            }
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    Ready,
    Exited,
    Timeout,
}

fn wait_service_outcome(state: &AppState, timeout: Duration) -> WaitOutcome {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if state.supervisor.lock().unwrap().health_check() {
            return WaitOutcome::Ready;
        }
        if state.supervisor.lock().unwrap().is_exited() {
            return WaitOutcome::Exited;
        }
        if std::time::Instant::now() >= deadline {
            return WaitOutcome::Timeout;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn service_url(state: &AppState) -> String {
    state
        .supervisor
        .lock()
        .unwrap()
        .captured_url()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", state.port()))
}

fn preferred_port(state: &AppState) -> u16 {
    let p = state.config.lock().unwrap().get().port;
    if p > 0 { p } else { 3080 }
}

fn wait_service_url(state: &AppState, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if state.supervisor.lock().unwrap().captured_url().is_some() {
            return;
        }
        if state.supervisor.lock().unwrap().is_exited() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct BootIssue {
    kind: serviceout::FailureKind,
    plugins: Vec<String>,
    lock_path: Option<String>,
    detail: String,
    spawn_error: bool,
}

fn boot_issue(state: &AppState) -> BootIssue {
    let kind = state
        .supervisor
        .lock()
        .unwrap()
        .failure_classify()
        .unwrap_or(serviceout::FailureKind::Unknown);
    let tail = state.supervisor.lock().unwrap().output_tail(4096);
    let mut issue = BootIssue {
        kind,
        plugins: Vec::new(),
        lock_path: None,
        detail: tail.clone(),
        spawn_error: false,
    };
    match &issue.kind {
        serviceout::FailureKind::PluginTree => {
            let candidates = serviceout::plugin_candidates(&tail);
            issue.plugins = plugins::removable_plugin_names(&candidates);
        }
        serviceout::FailureKind::StaleLock { lock_path } => {
            issue.lock_path = lock_path.clone();
        }
        serviceout::FailureKind::Unknown => {}
    }
    issue
}

fn emit_error_options(app: &AppHandle, state: &AppState, issue: &BootIssue) {
    state.set_last_recovery(RecoveryInfo {
        kind: issue.kind.kind_str().to_string(),
        plugins: issue.plugins.clone(),
        lock_path: issue.lock_path.clone(),
        detail: issue.detail.clone(),
    });
    let _ = app.emit(
        "boot://error-options",
        serde_json::json!({
            "kind": issue.kind.kind_str(),
            "plugins": issue.plugins,
            "lock_path": issue.lock_path,
            "detail": issue.detail,
        }),
    );
}

fn spawn_service_watch(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let guard_state = app.state::<AppState>();
        if guard_state
            .service_watch_active
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        drop(guard_state);
        std::thread::sleep(Duration::from_secs(3));
        loop {
            let state = app.state::<AppState>();
            if state.phase() != BootPhase::Ready {
                state.service_watch_active.store(false, Ordering::SeqCst);
                return;
            }
            let healthy = state.supervisor.lock().unwrap().health_check();
            if healthy {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            let exited = state.supervisor.lock().unwrap().is_exited();
            if !exited {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            let issue = boot_issue(&state);
            let msg = match &issue.kind {
                serviceout::FailureKind::PluginTree => {
                    "服务因插件不兼容意外停止，请移除不兼容插件后重试。"
                }
                serviceout::FailureKind::StaleLock { .. } => {
                    "服务意外停止（残留锁文件可能仍在阻止启动），请重试。"
                }
                serviceout::FailureKind::Unknown => "服务意外停止，请点击重试。",
            };
            state.service_watch_active.store(false, Ordering::SeqCst);
            let _ = app.emit(
                "boot://progress",
                serde_json::json!({ "phase": BootPhase::Error, "message": msg }),
            );
            state.set_error(msg);
            emit_error_options(&app, &state, &issue);
            log::warn!(
                "service crashed after ready ({}): {}",
                issue.kind.kind_str(),
                issue.detail
            );
            if let Some(w) = app.get_webview_window("main") {
                let on_service = w
                    .url()
                    .ok()
                    .map(|u| u.host_str() == Some("127.0.0.1"))
                    .unwrap_or(false);
                if on_service {
                    if let Some(bp) = state.boot_page_url() {
                        if let Ok(u) = bp.parse::<tauri::Url>() {
                            log::info!("service crashed; main window -> boot page");
                            let _ = w.navigate(u);
                        }
                    }
                }
            }
            update_tray_tooltip(&app);
            return;
        }
    });
}

fn is_safe_lock_path(path: &Path) -> bool {
    let name_ok = path
        .file_name()
        .map(|f| f.to_string_lossy().ends_with(".lock"))
        .unwrap_or(false);
    if !name_ok {
        return false;
    }
    let home = plugins::dsh_home();
    let p = path.to_string_lossy();
    let h = home.to_string_lossy();
    let (p, h) = if cfg!(windows) {
        (p.to_lowercase(), h.to_lowercase())
    } else {
        (p.into_owned(), h.into_owned())
    };
    p.starts_with(&h)
        && p[h.len()..]
            .chars()
            .next()
            .map(|c| c == '\\' || c == '/')
            .unwrap_or(false)
}

fn start_service_with_heal(
    app: &AppHandle,
    state: &AppState,
    node: &Path,
    dsh_bin: &Path,
    workspace: &Path,
    preferred: u16,
    extra: &[String],
    first_spawn: bool,
) -> Result<(), BootIssue> {
    let mut healed = false;
    let mut first = first_spawn;
    loop {
        let port = {
            let mut sup = state.supervisor.lock().unwrap();
            if first {
                sup.start(node, dsh_bin, workspace, preferred, extra)
            } else {
                sup.restart(node, dsh_bin, workspace, preferred, extra)
            }
        };
        first = false;
        let port = match port {
            Ok(p) => p,
            Err(e) => {
                return Err(BootIssue {
                    kind: serviceout::FailureKind::Unknown,
                    plugins: Vec::new(),
                    lock_path: None,
                    detail: e,
                    spawn_error: true,
                });
            }
        };
        state.set_port(port);
        log::info!("service on 127.0.0.1:{port}");

        match wait_service_outcome(state, Duration::from_secs(30)) {
            WaitOutcome::Ready => return Ok(()),
            _ => {}
        }
        let issue = boot_issue(state);
        if !healed {
            if let serviceout::FailureKind::StaleLock { lock_path: Some(path) } = &issue.kind {
                let p = PathBuf::from(path);
                if is_safe_lock_path(&p) {
                    healed = true;
                    match std::fs::remove_file(&p) {
                        Ok(_) => {
                            log::warn!("auto-removed stale lock: {path}");
                            let msg = if is_zh_locale() {
                                "已自动清理残留锁文件，正在重试…"
                            } else {
                                "Cleaned up a stale lock file, retrying…"
                            };
                            emit_progress(app, BootPhase::ServiceStart, msg);
                            continue;
                        }
                        Err(e) => log::warn!("remove stale lock {path}: {e}"),
                    }
                }
            }
        }
        return Err(issue);
    }
}

fn restart_dsh_service(
    app: &AppHandle,
    state: &AppState,
    node: &Path,
    runtime: &Path,
    workspace: &Path,
    extra: &[String],
) {
    let dsh_bin = runtime
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    let preferred = preferred_port(state);
    match start_service_with_heal(app, state, node, &dsh_bin, workspace, preferred, extra, true) {
        Ok(()) => {
            wait_service_url(state, Duration::from_secs(2));
            state.set_phase(BootPhase::Ready);
            state.clear_error();
            log::info!("service restarted after dsh update");
            spawn_service_watch(app);
        }
        Err(issue) => {
            state.set_error("更新后服务没有正常启动，请稍后再试。");
            log::warn!(
                "service not healthy after dsh update ({}): {}",
                issue.kind.kind_str(),
                issue.detail
            );
        }
    }
}

#[tauri::command]
async fn restart_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
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
    let node = state.node_path();
    let workspace = state.workspace_dir();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let preferred = preferred_port(&state);
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        match start_service_with_heal(&app2, &state, &node, &dsh_bin, &workspace, preferred, &extra, false) {
            Ok(()) => {
                wait_service_url(&state, Duration::from_secs(2));
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                plugins::record_restart(&state);
                refresh_main_after_restart(&app2, &state);
                Ok(())
            }
            Err(issue) => {
                if issue.spawn_error {
                    return Err(issue.detail);
                }
                let zh = is_zh_locale();
                let text: &str = match &issue.kind {
                    serviceout::FailureKind::PluginTree => {
                        if zh {
                            "插件与当前 DSH 版本不兼容，服务无法启动。请重新打开应用，在启动页移除不兼容插件。"
                        } else {
                            "Some plugins are incompatible with this version of DSH. Reopen the app to remove them on the startup screen."
                        }
                    }
                    serviceout::FailureKind::StaleLock { .. } => {
                        if zh {
                            "服务没有正常启动（残留锁文件可能仍在阻止启动），请稍后再试。"
                        } else {
                            "Service did not start (a leftover lock file may still block it). Please retry."
                        }
                    }
                    serviceout::FailureKind::Unknown => {
                        if zh {
                            "服务没有正常启动，请稍后再试。"
                        } else {
                            "Service did not start. Please retry."
                        }
                    }
                };
                state.set_error(text);
                log::warn!(
                    "restart_service failed ({}): {}",
                    issue.kind.kind_str(),
                    issue.detail
                );
                Err(text.into())
            }
        }
    })
    .await
    .map_err(|e| format!("重启任务异常: {e}"))?
}

#[tauri::command]
async fn repair_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer_js = resource_dir.join("installer").join("install-dsh.mjs");
    let baseline_dir = resource_dir.join("dsh-baseline");
    let runtime_dir = state.runtime_dir();
    let node_path = state.node_path();
    let workspace = state.workspace_dir();
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();

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

        let dsh_bin = runtime_dir
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        if !dsh_bin.exists() {
            return Err("修复后服务仍不完整，请重新安装。".into());
        }
        let preferred = preferred_port(&state);
        let extra = state.dsh_extra_args.lock().unwrap().clone();
        match start_service_with_heal(&app2, &state, &node_path, &dsh_bin, &workspace, preferred, &extra, true) {
            Ok(()) => {
                wait_service_url(&state, Duration::from_secs(2));
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                plugins::record_restart(&state);
                update_tray_tooltip(&app2);
                refresh_main_after_restart(&app2, &state);
                Ok(())
            }
            Err(issue) => {
                if issue.spawn_error {
                    return Err(issue.detail);
                }
                state.set_error("修复后服务没有正常启动，请稍后再试。");
                log::warn!(
                    "repair_service start failed ({}): {}",
                    issue.kind.kind_str(),
                    issue.detail
                );
                Err("服务未就绪".into())
            }
        }
    })
    .await
    .map_err(|e| format!("修复任务异常: {e}"))?
}

#[tauri::command]
fn get_app_update_status(state: State<'_, AppState>) -> Option<DshUpdateStatus> {
    state.app_update()
}

fn updater_configured(app: &AppHandle) -> bool {
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
    configured && !placeholder
}

#[tauri::command]
async fn check_app_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DshUpdateStatus, String> {
    use tauri_plugin_updater::UpdaterExt;

    let current = app.package_info().version.to_string();
    if !updater_configured(&app) {
        let status = DshUpdateStatus {
            ok: true,
            update_available: false,
            current: Some(current),
            latest: None,
            message: "未配置应用更新源".into(),
            ..Default::default()
        };
        state.set_app_update(status.clone());
        log::info!("updater: not configured, skipping check");
        return Ok(status);
    }

    let updater = app.updater().map_err(|e| e.to_string())?;
    let check = updater.check().await.map_err(|e| e.to_string())?;
    let status = match check {
        Some(update) => DshUpdateStatus {
            ok: true,
            update_available: true,
            current: Some(current.clone()),
            latest: Some(update.version.to_string()),
            message: format!("发现新版本 {}", update.version),
            ..Default::default()
        },
        None => DshUpdateStatus {
            ok: true,
            update_available: false,
            current: Some(current),
            latest: None,
            message: "已是最新版本".into(),
            ..Default::default()
        },
    };
    state.set_app_update(status.clone());
    log::info!("updater check: {:?}", status.message);
    Ok(status)
}

#[tauri::command]
async fn update_app(app: AppHandle, state: State<'_, AppState>) -> Result<DshUpdateStatus, String> {
    use tauri_plugin_updater::UpdaterExt;

    let current = app.package_info().version.to_string();
    if !updater_configured(&app) {
        let status = DshUpdateStatus {
            ok: false,
            update_available: false,
            current: Some(current),
            latest: None,
            message: "未配置应用更新源".into(),
            ..Default::default()
        };
        state.set_app_update(status.clone());
        return Ok(status);
    }

    let updater = app.updater().map_err(|e| e.to_string())?;
    let check = updater.check().await.map_err(|e| e.to_string())?;
    let Some(update) = check else {
        let status = DshUpdateStatus {
            ok: true,
            update_available: false,
            current: Some(current),
            latest: None,
            message: "已是最新版本".into(),
            ..Default::default()
        };
        state.set_app_update(status.clone());
        return Ok(status);
    };
    let latest = update.version.to_string();
    log::info!("updater: downloading {latest}");
    let status = match update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
    {
        Ok(_) => {
            log::info!("updater: installed {latest}");
            DshUpdateStatus {
                ok: true,
                update_available: false,
                current: Some(latest.clone()),
                latest: Some(latest),
                message: "已安装，重启应用后生效".into(),
                ..Default::default()
            }
        }
        Err(e) => {
            log::warn!("updater install failed: {e}");
            DshUpdateStatus {
                ok: false,
                update_available: true,
                current: Some(current),
                latest: Some(latest),
                message: format!("更新失败：{e}"),
                ..Default::default()
            }
        }
    };
    state.set_app_update(status.clone());
    Ok(status)
}

#[derive(serde::Serialize)]
struct SettingsData {
    app_version: String,
    node_version: Option<String>,
    dsh_version: Option<String>,
    port: u16,
    service_running: bool,
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

#[tauri::command]
fn open_log_dir(state: State<'_, AppState>) -> Result<(), String> {
    let log_file = state.log_file();
    let dir = log_file
        .parent()
        .ok_or("无法确定日志位置")?;
    open::that(dir).map_err(|e| format!("打开日志目录失败: {e}"))?;
    Ok(())
}

#[tauri::command]
fn open_workspace_dir(state: State<'_, AppState>) -> Result<(), String> {
    let ws = state.workspace_dir();
    if ws.as_os_str().is_empty() {
        return Err("工作区尚未初始化".into());
    }
    open::that(&ws).map_err(|e| format!("打开工作区失败: {e}"))?;
    Ok(())
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> Result<config::AppConfig, String> {
    Ok(state.config.lock().unwrap().get())
}

#[tauri::command]
fn set_config(state: State<'_, AppState>, config: config::AppConfig) -> Result<(), String> {
    state.config.lock().unwrap().set(config)
}

#[tauri::command]
fn get_dsh_update_status(state: State<'_, AppState>) -> Option<DshUpdateStatus> {
    state.dsh_update()
}

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
    let cfg = state.config.lock().unwrap().get();
    let registry_source = cfg.registry_source;
    let pre_release = cfg.pre_release;
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let status = match node::run_check(&node, &installer, &runtime, &registry_source, pre_release) {
            Ok(out) => {
                let r = node::parse_installer_output(&out);
                if !r.ok {
                    DshUpdateStatus {
                        ok: false,
                        update_available: false,
                        current,
                        latest: None,
                        message: r.message.unwrap_or_else(|| "暂时无法检查更新".into()),
                        ..Default::default()
                    }
                } else if r.action == "new-version-available" || r.action == "prerelease-available" {
                    let ver = r.version.clone().or_else(|| r.prerelease.clone());
                    let msg = if r.action == "prerelease-available" {
                        format!("发现预发布版本 {}", r.prerelease.clone().unwrap_or_default())
                    } else {
                        format!("发现新版本 {}", ver.unwrap_or_default())
                    };
                    DshUpdateStatus {
                        ok: true,
                        update_available: true,
                        current,
                        latest: r.version.clone(),
                        prerelease: r.prerelease.clone(),
                        pre_available: r.pre_available,
                        message: msg,
                    }
                } else {
                    let ver = r.version.clone().or_else(|| r.prerelease.clone());
                    DshUpdateStatus {
                        ok: true,
                        update_available: false,
                        current,
                        latest: r.version.clone(),
                        prerelease: r.prerelease.clone(),
                        pre_available: r.pre_available,
                        message: up_to_date_message(&ver),
                    }
                }
            }
            Err(e) => DshUpdateStatus {
                ok: false,
                update_available: false,
                current,
                latest: None,
                message: format!("检查更新失败：{e}"),
                ..Default::default()
            },
        };
        app2.state::<AppState>().set_dsh_update(status.clone());
        status
    })
    .await
    .map_err(|e| format!("检查更新任务异常: {e}"))?)
}

#[tauri::command]
async fn update_dsh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DshUpdateStatus, String> {
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer = resource_dir.join("installer").join("install-dsh.mjs");
    let cfg = state.config.lock().unwrap().get();
    let registry_source = cfg.registry_source;
    let pre_release = cfg.pre_release;
    let node = state.node_path();
    let workspace = state.workspace_dir();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let u = match node::run_update(&node, &installer, &runtime, &registry_source, pre_release) {
            Ok(out) => node::parse_installer_output(&out),
            Err(e) => {
                let status = DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: None,
                    message: format!("更新失败：{e}"),
                    ..Default::default()
                };
                state.set_dsh_update(status.clone());
                return status;
            }
        };
        if !u.ok || u.action != "downloaded" {
            let status = DshUpdateStatus {
                ok: u.ok,
                update_available: false,
                current,
                latest: u.version.clone(),
                prerelease: u.prerelease.clone(),
                pre_available: u.pre_available,
                message: u.message.unwrap_or_else(|| "更新失败，已保留当前版本。".into()),
            };
            state.set_dsh_update(status.clone());
            return status;
        }
        log::info!("update_dsh: downloaded {} to staging, swapping", u.version.clone().unwrap_or_default());
        state.supervisor.lock().unwrap().stop();
        let staging = match &u.staging {
            Some(s) => std::path::PathBuf::from(s),
            None => {
                let status = DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current: current.clone(),
                    latest: u.version.clone(),
                    message: "更新缺少暂存目录，已保留当前版本。".into(),
                    ..Default::default()
                };
                state.set_dsh_update(status.clone());
                restart_dsh_service(&app2, &state, &node, &runtime, &workspace, &extra);
                return status;
            }
        };
        let swap_out = node::run_swap(&node, &installer, &runtime, &staging);
        let status = match swap_out {
            Ok(out) => {
                let sw = node::parse_installer_output(&out);
                let new_version = sw.version.clone();
                let message = if sw.ok && sw.action == "updated" {
                    format!(
                        "已更新到 {}（重启服务后生效）",
                        new_version.clone().unwrap_or_default()
                    )
                } else {
                    sw.message.unwrap_or_else(|| "更新失败，已保留当前版本。".into())
                };
                DshUpdateStatus {
                    ok: sw.ok,
                    update_available: false,
                    current: sw.current.or(current.clone()),
                    latest: new_version,
                    prerelease: sw.prerelease.clone(),
                    pre_available: sw.pre_available,
                    message,
                }
            }
            Err(e) => DshUpdateStatus {
                ok: false,
                update_available: false,
                current: current.clone(),
                latest: u.version.clone(),
                message: format!("更新替换失败：{e}"),
                ..Default::default()
            },
        };
        state.set_dsh_update(status.clone());
        restart_dsh_service(&app2, &state, &node, &runtime, &workspace, &extra);
        status
    })
    .await
    .map_err(|e| format!("更新任务异常: {e}"))?)
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
                menu_restart(app, "ctx");
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
            if let tauri::webview::PageLoadEvent::Finished = payload.event() {
                if payload.url().scheme() == "http" && webview.label() == "main" {
                    if let Err(e) = webview.eval(include_str!("../assets/ctx-menu.js")) {
                        log::warn!("ctx script eval failed: {e}");
                    }
                    if payload.url().host_str() == Some("127.0.0.1") {
                        if let Err(e) = webview.eval(include_str!("../assets/notify-shim.js")) {
                            log::warn!("notify shim eval failed: {e}");
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
        })
        .on_window_event(|window, event| {
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
            get_settings,
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
            plugins::plugins_marketplace
        ])
        .run(tauri::generate_context!())
        .expect("error while running dsh-desktop");
}
