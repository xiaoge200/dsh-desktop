use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::node;
use crate::plugins;
use crate::service::{ops_guard, restart_dsh_service};
use crate::state::{AppState, BootPhase, DshUpdateStatus};
use crate::update_tray_tooltip;
use tauri::{AppHandle, Manager, State};

fn locked_file_hint() -> &'static str {
    "。若提示文件被占用（其他 DSH 窗口、杀毒或残留进程），请稍后重试，持续失败请重启电脑。"
}

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
    let guard = ops_guard();
    state.set_phase(BootPhase::ServiceStart);
    state.supervisor.lock().unwrap().ensure_stopped();
    update_tray_tooltip(app);
    let status = match &u.staging {
        Some(s) => {
            let staging = PathBuf::from(s);
            match node::run_swap(node, installer, runtime, &staging) {
                Ok(out) => {
                    let sw = node::parse_installer_output(&out);
                    let new_version = sw.version.clone();
                    let message = if sw.ok && sw.action == "updated" {
                        format!(
                            "已更新到 {}，服务已自动重启",
                            new_version.clone().unwrap_or_default()
                        )
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
    restart_dsh_service(app, state, node, runtime, workspace, extra);
    drop(guard);
    plugins::record_restart(state);
    status
}

pub(crate) fn spawn_bg_dsh_update(app: &AppHandle, installer_js: &Path) {
    let bg_app = app.clone();
    let bg_installer = installer_js.to_path_buf();
    std::thread::spawn(move || {
        let state = bg_app.state::<AppState>();
        let bg_node = state.node_path();
        let bg_runtime = state.runtime_dir();
        let bg_workspace = state.workspace_dir();
        let bg_extra = state.dsh_extra_args.lock().unwrap().clone();
        let cfg = state.config.lock().unwrap().get();
        let auto_update = cfg.auto_update_dsh;
        let registry_source = cfg.registry_source;
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
        let check = match node::run_check(&bg_node, &bg_installer, &bg_runtime, &registry_source, false) {
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
        let update_out = match node::run_update(&bg_node, &bg_installer, &bg_runtime, &registry_source, false) {
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
        let status = apply_swap(
            &bg_app,
            &state,
            &bg_node,
            &bg_installer,
            &bg_runtime,
            &bg_workspace,
            &bg_extra,
            &u,
            current,
        );
        state.set_dsh_update(status.clone());
        log::info!("bg update result: {}", status.message);
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

#[tauri::command]
pub(crate) async fn check_dsh_update(
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
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let status = match node::run_check(&node, &installer, &runtime, &registry_source, false) {
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
pub(crate) async fn update_dsh(
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
    let node = state.node_path();
    let workspace = state.workspace_dir();
    let extra = state.dsh_extra_args.lock().unwrap().clone();
    let current = node::read_installed_version(&runtime);
    let app2 = app.clone();
    Ok(tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let u = match node::run_update(&node, &installer, &runtime, &registry_source, false) {
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
        state.set_dsh_update(status.clone());
        status
    })
    .await
    .map_err(|e| format!("更新任务异常: {e}"))?)
}
