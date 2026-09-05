use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::state::{AppState, BootPhase, RecoveryInfo};
use crate::{emit_progress, is_zh_locale, node, plugins, serviceout, update_tray_tooltip};
use tauri::{AppHandle, Emitter, Manager, State};

const EXIT_TAIL_DRAIN: Duration = Duration::from_millis(500);
const PROBE_TICK: Duration = Duration::from_millis(500);
const PROBE_WINDOW: Duration = Duration::from_secs(20);
const HEALTH_REAP_EVERY: u32 = 15;

// 锁序约定：只允许 OPS_LOCK → supervisor 短锁；持 supervisor 锁时禁止阻塞取 OPS_LOCK。
pub(crate) static OPS_LOCK: Mutex<()> = Mutex::new(());
pub(crate) static OPS_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

static UI_SYNC_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static UI_SYNC_TX: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);

// 启动一个持有 AppHandle 的 UI 协调线程：所有状态变更（phase/port/error/
// 互斥操作）只发信号，由它统一刷新托盘 tooltip 与重启菜单项——单一出口，
// 避免各调用点手工同步不一致（tauri 句柄不可放入 static/托管状态，见记忆）。
pub(crate) fn start_ui_sync(app: AppHandle) {
    let (tx, rx) = mpsc::channel::<()>();
    *UI_SYNC_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    std::thread::spawn(move || loop {
        if rx.recv().is_err() {
            return;
        }
        loop {
            match rx.try_recv() {
                Ok(()) => continue,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        UI_SYNC_PENDING.store(false, Ordering::SeqCst);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            update_tray_tooltip(&app);
        }));
    });
}

pub(crate) fn request_ui_sync() {
    UI_SYNC_PENDING.store(true, Ordering::SeqCst);
    if let Some(tx) = UI_SYNC_TX.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        let _ = tx.send(());
    }
}

pub(crate) fn notify_service_ui() {
    request_ui_sync();
}

pub(crate) struct OpGuard {
    _lock: MutexGuard<'static, ()>,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        OPS_ACTIVE.fetch_sub(1, Ordering::SeqCst);
        request_ui_sync();
    }
}

pub(crate) fn ops_guard() -> OpGuard {
    let lock = OPS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    OPS_ACTIVE.fetch_add(1, Ordering::SeqCst);
    request_ui_sync();
    OpGuard { _lock: lock }
}

pub(crate) fn ops_active() -> usize {
    OPS_ACTIVE.load(Ordering::SeqCst)
}

pub(crate) fn restart_action_allowed(state: &AppState) -> bool {
    if ops_active() > 0 {
        return false;
    }
    let phase = state.phase();
    phase == BootPhase::Ready || phase == BootPhase::Error
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

fn restart_with_heal(
    app: &AppHandle,
    state: &AppState,
    tag: &'static str,
    first_spawn: bool,
) -> Result<(), BootIssue> {
    let runtime_dir = state.runtime_dir();
    let dsh_bin = node::dsh_bin(&runtime_dir);
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let preferred = preferred_port(state);
    match start_service_with_heal(
        app,
        state,
        &state.node_path(),
        &dsh_bin,
        &state.workspace_dir(),
        preferred,
        &extra,
        first_spawn,
    ) {
        Ok(()) => {
            wait_service_url(state, Duration::from_secs(2));
            state.set_phase(BootPhase::Ready);
            state.clear_error();
            log::info!("{tag} restart ready: {}", service_url(state));
            Ok(())
        }
        Err(issue) => Err(issue),
    }
}

fn mutation_failure_text(kind: &serviceout::FailureKind) -> &'static str {
    let zh = is_zh_locale();
    match kind {
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
    }
}

pub(crate) fn run_exclusive_mutation<T>(
    app: &AppHandle,
    state: &AppState,
    op_label: &str,
    op: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mutation_inner(app, state, op_label, op)
    }));
    match outcome {
        Ok(r) => r,
        Err(_) => {
            log::error!("{op_label}: panicked during exclusive mutation");
            state.supervisor.lock().unwrap().ensure_stopped();
            state.set_error("服务操作异常中断，请重试。");
            update_tray_tooltip(app);
            Err("服务操作异常中断，请重试。".into())
        }
    }
}

fn mutation_inner<T>(
    app: &AppHandle,
    state: &AppState,
    op_label: &str,
    op: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let guard = ops_guard();
    log::info!("{op_label}: exclusive mutation starts (service will be stopped)");
    state.set_phase(BootPhase::ServiceStart);
    state.supervisor.lock().unwrap().ensure_stopped();
    update_tray_tooltip(app);
    let op_result = op();
    let node = state.node_path();
    let runtime = state.runtime_dir();
    let dsh_bin = node::dsh_bin(&runtime);
    let workspace = state.workspace_dir();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let preferred = preferred_port(state);
    let start_result = start_service_with_heal(
        app,
        state,
        &node,
        &dsh_bin,
        &workspace,
        preferred,
        &extra,
        true,
    );
    match &start_result {
        Ok(()) => {
            wait_service_url(state, Duration::from_secs(2));
            state.set_phase(BootPhase::Ready);
            state.clear_error();
            plugins::record_restart_force(state);
            drop(guard);
            update_tray_tooltip(app);
            refresh_main_after_restart(app, state);
        }
        Err(issue) => {
            log::warn!("{op_label}: service restart failed ({}): {}", issue.kind.kind_str(), issue.detail);
            if issue.spawn_error {
                fail_to_boot_page(app, state, issue, &issue.detail);
            } else {
                fail_to_boot_page(app, state, issue, mutation_failure_text(&issue.kind));
            }
        }
    }
    match (op_result, start_result) {
        (Ok(v), Ok(())) => Ok(v),
        (Err(e), Ok(())) => Err(format!("{e}\n服务已自动重启。")),
        (Ok(_), Err(_)) => Err("操作已完成，但服务未能自动重启。".into()),
        (Err(e), Err(_)) => Err(format!("{e}\n服务也未能自动重启。")),
    }
}

pub(crate) fn menu_restart(app: &AppHandle, tag: &'static str) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        if state.runtime_dir().as_os_str().is_empty() {
            return;
        }
        let outcome = {
            let _guard = ops_guard();
            update_tray_tooltip(&app);
            match restart_with_heal(&app, &state, tag, true) {
                Ok(()) => {
                    refresh_main_after_restart(&app, &state);
                    Ok(())
                }
                Err(issue) => {
                    log::warn!(
                        "{tag} restart failed ({}): {}",
                        issue.kind.kind_str(),
                        issue.detail
                    );
                    if !issue.spawn_error {
                        fail_to_boot_page(&app, &state, &issue, crash_message(&issue.kind));
                    }
                    Err(())
                }
            }
        };
        update_tray_tooltip(&app);
        let _ = outcome;
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
        if state.service_health() {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeDecision {
    BackToWatch,
    Takeover,
    TimedOut,
    KeepProbing,
}

fn probe_decision(child_alive: bool, port_healthy: bool, elapsed: Duration, window: Duration) -> ProbeDecision {
    if child_alive {
        return ProbeDecision::BackToWatch;
    }
    if port_healthy {
        return ProbeDecision::Takeover;
    }
    if elapsed >= window {
        return ProbeDecision::TimedOut;
    }
    ProbeDecision::KeepProbing
}

fn crash_message(kind: &serviceout::FailureKind) -> &'static str {
    match kind {
        serviceout::FailureKind::PluginTree => {
            "服务因插件不兼容意外停止，请移除不兼容插件后重试。"
        }
        serviceout::FailureKind::StaleLock { .. } => {
            "服务意外停止（残留锁文件可能仍在阻止启动），请重试。"
        }
        serviceout::FailureKind::Unknown => "服务意外停止，请点击重试。",
    }
}

fn navigate_main_to_boot(app: &AppHandle, state: &AppState) {
    if let Some(w) = app.get_webview_window("main") {
        let on_service = w
            .url()
            .ok()
            .map(|u| u.host_str() == Some("127.0.0.1"))
            .unwrap_or(false);
        if on_service {
            if let Some(bp) = state.boot_page_url() {
                if let Ok(u) = bp.parse::<tauri::Url>() {
                    log::info!("service down; main window -> boot page");
                    let _ = w.navigate(u);
                }
            }
        }
    }
}

fn fail_to_boot_page(app: &AppHandle, state: &AppState, issue: &BootIssue, msg: &str) {
    let _ = app.emit(
        "boot://progress",
        serde_json::json!({ "phase": BootPhase::Error, "message": msg }),
    );
    state.set_error(msg);
    emit_error_options(app, state, issue);
    log::warn!(
        "service crashed after ready ({}): {}",
        issue.kind.kind_str(),
        issue.detail
    );
    navigate_main_to_boot(app, state);
    update_tray_tooltip(app);
}

fn fail_watch(app: &AppHandle, state: &AppState, issue: &BootIssue, msg: &str) {
    state.service_watch_active.store(false, Ordering::SeqCst);
    fail_to_boot_page(app, state, issue, msg);
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
        let mut reap_tick: u32 = 0;
        loop {
            reap_tick = reap_tick.wrapping_add(1);
            let state = app.state::<AppState>();
            if state.phase() != BootPhase::Ready {
                state.service_watch_active.store(false, Ordering::SeqCst);
                return;
            }
            let healthy = state.service_health();
            let exited = if healthy && reap_tick.is_multiple_of(HEALTH_REAP_EVERY) {
                state.supervisor.lock().unwrap().is_exited()
            } else if healthy {
                false
            } else {
                state.supervisor.lock().unwrap().is_exited()
            };
            if !exited {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            std::thread::sleep(EXIT_TAIL_DRAIN);
            let issue = boot_issue(&state);
            if issue.kind != serviceout::FailureKind::Unknown {
                fail_watch(&app, &state, &issue, crash_message(&issue.kind));
                return;
            }
            let zh = is_zh_locale();
            let msg = if zh {
                "服务正在自动恢复，请稍候…"
            } else {
                "Service stopped; recovering automatically…"
            };
            let _ = app.emit(
                "boot://progress",
                serde_json::json!({ "phase": BootPhase::ServiceStart, "message": msg }),
            );
            log::info!(
                "service exited; probing for external takeover on 127.0.0.1:{}",
                state.port()
            );
            let probe_start = Instant::now();
            let mut taken_over = false;
            loop {
                let state = app.state::<AppState>();
                if state.phase() != BootPhase::Ready {
                    state.service_watch_active.store(false, Ordering::SeqCst);
                    return;
                }
                let child_alive = !state.supervisor.lock().unwrap().is_exited();
                let healthy = state.service_health();
                match probe_decision(child_alive, healthy, probe_start.elapsed(), PROBE_WINDOW) {
                    ProbeDecision::KeepProbing => std::thread::sleep(PROBE_TICK),
                    ProbeDecision::BackToWatch => break,
                    ProbeDecision::Takeover => {
                        taken_over = true;
                        break;
                    }
                    ProbeDecision::TimedOut => {
                        fail_watch(&app, &state, &issue, crash_message(&issue.kind));
                        return;
                    }
                }
            }
            if !taken_over {
                continue;
            }
            let state = app.state::<AppState>();
            if !state.supervisor.lock().unwrap().is_exited() {
                continue;
            }
            log::info!("external takeover on port {}; re-claiming", state.port());
            let outcome = {
                let _guard = ops_guard();
                update_tray_tooltip(&app);
                restart_with_heal(&app, &state, "auto-recover", false)
            };
            update_tray_tooltip(&app);
            match outcome {
                Ok(()) => {
                    state.service_watch_active.store(false, Ordering::SeqCst);
                    refresh_main_after_restart(&app, &state);
                    return;
                }
                Err(issue2) => {
                    state.supervisor.lock().unwrap().ensure_stopped();
                    fail_watch(&app, &state, &issue2, crash_message(&issue2.kind));
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_decision_child_alive_returns_to_watch() {
        assert_eq!(
            probe_decision(true, true, Duration::from_secs(1), PROBE_WINDOW),
            ProbeDecision::BackToWatch
        );
        assert_eq!(
            probe_decision(true, false, Duration::from_secs(30), PROBE_WINDOW),
            ProbeDecision::BackToWatch
        );
    }

    #[test]
    fn probe_decision_child_dead_healthy_port_is_takeover() {
        assert_eq!(
            probe_decision(false, true, Duration::from_secs(1), PROBE_WINDOW),
            ProbeDecision::Takeover
        );
        assert_eq!(
            probe_decision(false, true, Duration::from_secs(30), PROBE_WINDOW),
            ProbeDecision::Takeover
        );
    }

    #[test]
    fn probe_decision_keeps_probing_inside_window() {
        assert_eq!(
            probe_decision(false, false, Duration::from_secs(5), PROBE_WINDOW),
            ProbeDecision::KeepProbing
        );
    }

    #[test]
    fn probe_decision_times_out_at_and_after_window() {
        assert_eq!(
            probe_decision(false, false, PROBE_WINDOW, PROBE_WINDOW),
            ProbeDecision::TimedOut
        );
        assert_eq!(
            probe_decision(false, false, Duration::from_secs(25), PROBE_WINDOW),
            ProbeDecision::TimedOut
        );
    }

    #[test]
    fn crash_message_has_text_for_every_kind() {
        let kinds = [
            serviceout::FailureKind::PluginTree,
            serviceout::FailureKind::StaleLock {
                lock_path: Some("~/.dsh/x.lock".into()),
            },
            serviceout::FailureKind::Unknown,
        ];
        for kind in kinds {
            let msg = crash_message(&kind);
            assert!(!msg.is_empty());
            assert_eq!(msg, crash_message(&kind));
        }
    }

    #[test]
    fn mutation_failure_text_has_text_for_every_kind() {
        let kinds = [
            serviceout::FailureKind::PluginTree,
            serviceout::FailureKind::StaleLock {
                lock_path: Some("~/.dsh/x.lock".into()),
            },
            serviceout::FailureKind::Unknown,
        ];
        for kind in kinds {
            assert!(!mutation_failure_text(&kind).is_empty());
        }
    }
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

        match wait_service_outcome(state, Duration::from_secs(60)) {
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
) -> Result<(), BootIssue> {
    let dsh_bin = node::dsh_bin(runtime);
    let preferred = preferred_port(state);
    let result = match start_service_with_heal(app, state, node, &dsh_bin, workspace, preferred, extra, true) {
        Ok(()) => {
            wait_service_url(state, Duration::from_secs(2));
            state.set_phase(BootPhase::Ready);
            state.clear_error();
            log::info!("service restarted after dsh update");
            refresh_main_after_restart(app, state);
            Ok(())
        }
        Err(issue) => {
            state.set_error("更新后服务没有正常启动，请稍后再试。");
            log::warn!(
                "service not healthy after dsh update ({}): {}",
                issue.kind.kind_str(),
                issue.detail
            );
            Err(issue)
        }
    };
    update_tray_tooltip(app);
    result
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
        let guard = ops_guard();
        update_tray_tooltip(&app2);
        let state = app2.state::<AppState>();
        match start_service_with_heal(&app2, &state, &node, &dsh_bin, &workspace, preferred, &extra, true) {
            Ok(()) => {
                wait_service_url(&state, Duration::from_secs(2));
                state.set_phase(BootPhase::Ready);
                state.clear_error();
                drop(guard);
                plugins::record_restart(&state);
                refresh_main_after_restart(&app2, &state);
                update_tray_tooltip(&app2);
                Ok(())
            }
            Err(issue) => {
                if issue.spawn_error {
                    state.set_error(issue.detail.clone());
                    emit_error_options(&app2, &state, &issue);
                    navigate_main_to_boot(&app2, &state);
                    update_tray_tooltip(&app2);
                    return Err(issue.detail);
                }
                let text = mutation_failure_text(&issue.kind);
                state.set_error(text);
                emit_error_options(&app2, &state, &issue);
                navigate_main_to_boot(&app2, &state);
                update_tray_tooltip(&app2);
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
        let guard = ops_guard();
        update_tray_tooltip(&app2);
        let state = app2.state::<AppState>();

        state.supervisor.lock().unwrap().ensure_stopped();
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
                drop(guard);
                plugins::record_restart(&state);
                update_tray_tooltip(&app2);
                refresh_main_after_restart(&app2, &state);
                Ok(())
            }
            Err(issue) => {
                if issue.spawn_error {
                    fail_to_boot_page(&app2, &state, &issue, &issue.detail);
                    return Err(issue.detail);
                }
                let text = "修复后服务没有正常启动，请稍后再试。";
                fail_to_boot_page(&app2, &state, &issue, text);
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
