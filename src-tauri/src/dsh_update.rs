use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::node;
use crate::plugins;
use crate::service::{ops_guard, restart_dsh_service};
use crate::state::{
    AppState, BootPhase, DshUpdateStatus, UpdateProgress, UpdateProgressSnapshot, UpdateStage,
};
use crate::update_tray_tooltip;
use tauri::{AppHandle, Manager, State};

static DSH_UPDATE_ACTIVE: AtomicBool = AtomicBool::new(false);

struct UpdateActiveGuard;

impl Drop for UpdateActiveGuard {
    fn drop(&mut self) {
        DSH_UPDATE_ACTIVE.store(false, Ordering::SeqCst);
    }
}

fn try_begin_update() -> Option<UpdateActiveGuard> {
    DSH_UPDATE_ACTIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .ok()
        .map(|_| UpdateActiveGuard)
}

pub(crate) fn dsh_update_active() -> bool {
    DSH_UPDATE_ACTIVE.load(Ordering::SeqCst)
}

struct UpdateResetGuard {
    app: AppHandle,
}

impl Drop for UpdateResetGuard {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        state.dsh_cancel.store(false, Ordering::SeqCst);
        state.set_dsh_progress(UpdateProgress::default());
    }
}

fn set_stage(state: &AppState, stage: UpdateStage, can_cancel: bool) {
    state.set_dsh_progress(UpdateProgress {
        stage,
        can_cancel,
        ..Default::default()
    });
}

fn cancelled_status(prev: Option<DshUpdateStatus>, current: Option<String>) -> DshUpdateStatus {
    let p = prev.unwrap_or_default();
    DshUpdateStatus {
        ok: false,
        update_available: p.update_available,
        current: current.or(p.current),
        latest: p.latest,
        prerelease: p.prerelease,
        pre_available: p.pre_available,
        message: "已取消，可稍后重试。".into(),
    }
}

fn busy_status(state: &AppState) -> DshUpdateStatus {
    state
        .dsh_update()
        .unwrap_or_else(|| DshUpdateStatus {
            message: "更新正在进行中，请稍候。".into(),
            ..Default::default()
        })
}

fn locked_file_hint() -> &'static str {
    "。若提示文件被占用（其他 DSH 窗口、杀毒或残留进程），请稍后重试，持续失败请重启电脑。"
}

fn notify_dsh_update(app: &AppHandle, ok: bool, body: &str) {
    let body = body.replace(locked_file_hint(), "");
    let zh = crate::is_zh_locale();
    let title = match (ok, zh) {
        (true, true) => "DSH 更新完成",
        (true, false) => "DSH update finished",
        (false, true) => "DSH 更新失败",
        (false, false) => "DSH update failed",
    };
    crate::notify_update(app, title, &body);
}

fn notify_dsh_available(app: &AppHandle, body: &str) {
    let zh = crate::is_zh_locale();
    let title = if zh { "发现 DSH 新版本" } else { "New DSH version available" };
    crate::notify_update(app, title, body);
}

#[allow(clippy::too_many_arguments)]
fn apply_swap(
    app: &AppHandle,
    state: &AppState,
    node: &Path,
    installer: &Path,
    runtime: &Path,
    workspace: &Path,
    extra: &[String],
    u: &node::InstallerResult,
    previous_current: Option<String>,
) -> DshUpdateStatus {
    set_stage(state, UpdateStage::Swapping, false);
    let guard = ops_guard();
    state.set_phase(BootPhase::ServiceStart);
    state.supervisor.lock().unwrap().ensure_stopped();
    update_tray_tooltip(app);
    let mut status = match &u.staging {
        Some(s) => {
            let staging = PathBuf::from(s);
            match node::run_swap(node, installer, runtime, &staging) {
                Ok(out) => {
                    let sw = node::parse_installer_output(&out);
                    let new_version = sw.version.clone();
                    let message = if sw.ok && sw.action == "updated" {
                        format!("已更新到 {}", new_version.clone().unwrap_or_default())
                    } else if sw.ok {
                        sw.message.unwrap_or_else(|| "更新结果未知，请查看日志".into())
                    } else {
                        format!(
                            "{}{}",
                            sw.message.unwrap_or_else(|| "新版本替换失败".into()),
                            locked_file_hint()
                        )
                    };
                    DshUpdateStatus {
                        ok: sw.ok,
                        update_available: false,
                        current: sw.current.or(previous_current),
                        latest: new_version,
                        prerelease: sw.prerelease.clone(),
                        pre_available: sw.pre_available,
                        message,
                    }
                }
                Err(e) => DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current: previous_current,
                    latest: u.version.clone(),
                    message: format!("更新替换失败：{e}{}", locked_file_hint()),
                    ..Default::default()
                },
            }
        }
        None => DshUpdateStatus {
            ok: false,
            update_available: false,
            current: previous_current,
            latest: u.version.clone(),
            message: "更新缺少暂存目录，已保留当前版本。".into(),
            ..Default::default()
        },
    };
    set_stage(state, UpdateStage::Restarting, false);
    match restart_dsh_service(app, state, node, runtime, workspace, extra) {
        Ok(()) => {
            if status.ok && status.message.starts_with("已更新到") {
                status.message = format!(
                    "已更新到 {}，服务已自动重启",
                    status.latest.clone().unwrap_or_default()
                );
            }
        }
        Err(_) => {
            if status.ok {
                status.ok = false;
                status.message = format!(
                    "已更新到 {}，但服务未能自动重启，请稍后在设置页重启服务。",
                    status.latest.clone().unwrap_or_default()
                );
            }
        }
    }
    drop(guard);
    plugins::record_restart(state);
    update_tray_tooltip(app);
    status
}

async fn auto_update_once(app: AppHandle) {
    let state = app.state::<AppState>();
    let auto = state.config.lock().unwrap().get().auto_update_dsh;
    if !auto {
        let current = node::read_installed_version(&state.runtime_dir());
        state.set_dsh_update(DshUpdateStatus {
            ok: true,
            update_available: false,
            current,
            latest: None,
            ..Default::default()
        });
        log::info!("auto update: disabled by user");
        return;
    }
    match check_update_flow(app.clone()).await {
        Ok(st) if st.ok && st.update_available => {
            log::info!("auto update: new version available, applying");
            if let Err(e) = apply_update_flow(app).await {
                log::warn!("auto update apply failed: {e}");
            }
        }
        Ok(_) => log::info!("auto update: no new version"),
        Err(e) => log::warn!("auto update check failed: {e}"),
    }
}

pub(crate) fn spawn_bg_dsh_update(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(30));
        tauri::async_runtime::spawn(async move {
            auto_update_once(app).await;
        });
    });
}

fn up_to_date_message(version: &Option<String>) -> String {
    match version {
        Some(v) if !v.trim().is_empty() => format!("已是最新版本（{v}）"),
        _ => "已是最新版本".to_string(),
    }
}

#[tauri::command]
pub(crate) fn get_dsh_update_status(state: State<'_, AppState>) -> Option<DshUpdateStatus> {
    state.dsh_update()
}

async fn check_update_flow(app: AppHandle) -> Result<DshUpdateStatus, String> {
    let state = app.state::<AppState>();
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer = resource_dir.join("installer").join("install-dsh.mjs");
    let cfg = state.config.lock().unwrap().get();
    let registry_source = cfg.registry_source;
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let Some(_update_guard) = try_begin_update() else {
            log::info!("check update: skipped, another update is running");
            return busy_status(&state);
        };
        let _reset = UpdateResetGuard { app: app2.clone() };
        state.dsh_cancel.store(false, Ordering::SeqCst);
        set_stage(&state, UpdateStage::Checking, true);
        let prev = state.dsh_update();
        let status = match node::run_check(
            &node,
            &installer,
            &runtime,
            &registry_source,
            false,
            Some(&state.dsh_cancel),
        ) {
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
            Err(e) if node::is_cancelled(&e) => cancelled_status(prev, current),
            Err(e) => DshUpdateStatus {
                ok: false,
                update_available: false,
                current,
                latest: None,
                message: format!("检查更新失败：{e}"),
                ..Default::default()
            },
        };
        if status.ok && status.update_available {
            notify_dsh_available(&app2, &status.message);
        }
        state.set_dsh_update(status.clone());
        status
    })
    .await
    .map_err(|e| format!("检查更新任务异常: {e}"))
}

#[tauri::command]
pub(crate) async fn check_dsh_update(app: AppHandle) -> Result<DshUpdateStatus, String> {
    check_update_flow(app).await
}

async fn apply_update_flow(app: AppHandle) -> Result<DshUpdateStatus, String> {
    let state = app.state::<AppState>();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    let installer = resource_dir.join("installer").join("install-dsh.mjs");
    let cfg = state.config.lock().unwrap().get();
    let registry_source = cfg.registry_source;
    let node = state.node_path();
    let workspace = state.workspace_dir();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let Some(_update_guard) = try_begin_update() else {
            log::info!("update dsh: skipped, another update is running");
            return busy_status(&state);
        };
        let _reset = UpdateResetGuard { app: app2.clone() };
        state.dsh_cancel.store(false, Ordering::SeqCst);
        set_stage(&state, UpdateStage::Downloading, true);
        let prev = state.dsh_update();
        let u = match node::run_update(
            &node,
            &installer,
            &runtime,
            &registry_source,
            false,
            Some(&state.dsh_cancel),
        ) {
            Ok(out) => node::parse_installer_output(&out),
            Err(e) if node::is_cancelled(&e) => {
                let status = cancelled_status(prev, current);
                state.set_dsh_update(status.clone());
                return status;
            }
            Err(e) => {
                let status = DshUpdateStatus {
                    ok: false,
                    update_available: false,
                    current,
                    latest: None,
                    message: format!("更新失败：{e}"),
                    ..Default::default()
                };
                notify_dsh_update(&app2, false, &status.message);
                state.set_dsh_update(status.clone());
                return status;
            }
        };
        if !u.ok {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current,
                latest: u.version.clone(),
                prerelease: u.prerelease.clone(),
                pre_available: u.pre_available,
                message: u.message.unwrap_or_else(|| "更新失败，已保留当前版本。".into()),
            };
            notify_dsh_update(&app2, false, &status.message);
            state.set_dsh_update(status.clone());
            return status;
        }
        if u.action != "downloaded" {
            let ver = u.version.clone().or_else(|| u.prerelease.clone());
            let status = DshUpdateStatus {
                ok: true,
                update_available: false,
                current,
                latest: u.version.clone(),
                prerelease: u.prerelease.clone(),
                pre_available: u.pre_available,
                message: up_to_date_message(&ver),
            };
            notify_dsh_update(&app2, true, &status.message);
            state.set_dsh_update(status.clone());
            return status;
        }
        state.dsh_cancel.store(false, Ordering::SeqCst);
        log::info!("update_dsh: downloaded {} to staging, swapping", u.version.clone().unwrap_or_default());
        let status = apply_swap(
            &app2,
            &state,
            &node,
            &installer,
            &runtime,
            &workspace,
            &extra,
            &u,
            current,
        );
        notify_dsh_update(&app2, status.ok, &status.message);
        state.set_dsh_update(status.clone());
        status
    })
    .await
    .map_err(|e| format!("更新任务异常: {e}"))
}

#[tauri::command]
pub(crate) async fn update_dsh(app: AppHandle) -> Result<DshUpdateStatus, String> {
    apply_update_flow(app).await
}

#[tauri::command]
pub(crate) fn get_update_progress(state: State<'_, AppState>) -> UpdateProgressSnapshot {
    state.progress_snapshot()
}

#[tauri::command]
pub(crate) fn cancel_dsh_update(state: State<'_, AppState>) {
    let p = state.dsh_progress();
    if p.can_cancel
        && (p.stage == UpdateStage::Checking || p.stage == UpdateStage::Downloading)
    {
        state.dsh_cancel.store(true, Ordering::SeqCst);
    }
}
