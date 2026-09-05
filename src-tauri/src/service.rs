use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::state::{AppState, BootPhase, RecoveryInfo};
use crate::{emit_progress, is_zh_locale, node, plugins, serviceout, update_tray_tooltip};
use tauri::{AppHandle, Emitter, Manager, State};

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

pub(crate) fn menu_restart(app: &AppHandle, tag: &'static str) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let runtime_dir = state.runtime_dir();
        if runtime_dir.as_os_str().is_empty() {
            return;
        }
        let dsh_bin = node::dsh_bin(&runtime_dir);
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

pub(crate) fn service_url(state: &AppState) -> String {
    state
        .supervisor
        .lock()
        .unwrap()
        .captured_url()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", state.port()))
}

pub(crate) fn preferred_port(state: &AppState) -> u16 {
    let p = state.config.lock().unwrap().get().port;
    if p > 0 { p } else { 3080 }
}

pub(crate) fn wait_service_url(state: &AppState, timeout: Duration) {
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

pub(crate) struct BootIssue {
    pub(crate) kind: serviceout::FailureKind,
    pub(crate) plugins: Vec<String>,
    pub(crate) lock_path: Option<String>,
    pub(crate) detail: String,
    pub(crate) spawn_error: bool,
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

pub(crate) fn emit_error_options(app: &AppHandle, state: &AppState, issue: &BootIssue) {
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

pub(crate) fn spawn_service_watch(app: &AppHandle) {
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

pub(crate) fn start_service_with_heal(
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

pub(crate) fn restart_dsh_service(
    app: &AppHandle,
    state: &AppState,
    node: &Path,
    runtime: &Path,
    workspace: &Path,
    extra: &[String],
) {
    let dsh_bin = node::dsh_bin(runtime);
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
pub(crate) async fn restart_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let runtime_dir = state.runtime_dir();
    if runtime_dir.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let dsh_bin = node::dsh_bin(&runtime_dir);
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
pub(crate) async fn repair_service(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
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

        let dsh_bin = node::dsh_bin(&runtime_dir);
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
