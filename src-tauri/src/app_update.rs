use crate::state::{AppState, DshUpdateStatus, UpdateProgress, UpdateStage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const CANCELLED: &str = "\u{1F}app-cancelled";

static APP_UPDATE_ACTIVE: AtomicBool = AtomicBool::new(false);

struct AppUpdateActiveGuard;

impl Drop for AppUpdateActiveGuard {
    fn drop(&mut self) {
        APP_UPDATE_ACTIVE.store(false, Ordering::SeqCst);
    }
}

fn try_begin_app_update() -> Option<AppUpdateActiveGuard> {
    APP_UPDATE_ACTIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .ok()
        .map(|_| AppUpdateActiveGuard)
}

struct AppResetGuard {
    app: AppHandle,
}

impl Drop for AppResetGuard {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        state.app_cancel.store(false, Ordering::SeqCst);
        state.set_app_progress(UpdateProgress::default());
    }
}

fn set_app_stage(app: &AppHandle, stage: UpdateStage, can_cancel: bool) {
    app.state::<AppState>().set_app_progress(UpdateProgress {
        stage,
        can_cancel,
        ..Default::default()
    });
}

async fn wait_cancelled(app: AppHandle) {
    loop {
        if app.state::<AppState>().app_cancel.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn notify_app(app: &AppHandle, title_zh: &str, title_en: &str, body: &str) {
    let zh = crate::is_zh_locale();
    crate::notify_update(app, if zh { title_zh } else { title_en }, body);
}

#[tauri::command]
pub(crate) fn get_app_update_status(state: State<'_, AppState>) -> Option<DshUpdateStatus> {
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
pub(crate) async fn check_app_update(
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

    let Some(_guard) = try_begin_app_update() else {
        log::info!("check app update: skipped, another update is running");
        return Ok(state
            .app_update()
            .unwrap_or_else(|| DshUpdateStatus {
                message: "更新正在进行中，请稍候。".into(),
                ..Default::default()
            }));
    };
    let _reset = AppResetGuard { app: app.clone() };
    state.app_cancel.store(false, Ordering::SeqCst);
    set_app_stage(&app, UpdateStage::Checking, false);

    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current: Some(current),
                latest: None,
                message: format!("检查更新失败：{e}"),
                ..Default::default()
            };
            notify_app(&app, "应用更新检查失败", "Update check failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
    };
    let check = match tokio::time::timeout(CHECK_TIMEOUT, updater.check()).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current: Some(current),
                latest: None,
                message: format!("检查更新失败：{e}"),
                ..Default::default()
            };
            notify_app(&app, "应用更新检查失败", "Update check failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
        Err(_) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current: Some(current),
                latest: None,
                message: "检查更新超时（已等待 60 秒），请检查网络后重试。".into(),
                ..Default::default()
            };
            notify_app(&app, "应用更新检查失败", "Update check failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
    };
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
    if status.update_available {
        notify_app(&app, "发现应用新版本", "New app version available", &status.message);
    }
    state.set_app_update(status.clone());
    log::info!("updater check: {:?}", status.message);
    Ok(status)
}

#[tauri::command]
pub(crate) async fn update_app(app: AppHandle, state: State<'_, AppState>) -> Result<DshUpdateStatus, String> {
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

    let Some(_guard) = try_begin_app_update() else {
        log::info!("update app: skipped, another update is running");
        return Ok(state
            .app_update()
            .unwrap_or_else(|| DshUpdateStatus {
                message: "更新正在进行中，请稍候。".into(),
                ..Default::default()
            }));
    };
    let _reset = AppResetGuard { app: app.clone() };
    state.app_cancel.store(false, Ordering::SeqCst);
    set_app_stage(&app, UpdateStage::Checking, false);

    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current: Some(current),
                latest: None,
                message: format!("更新失败：{e}"),
                ..Default::default()
            };
            notify_app(&app, "应用更新失败", "App update failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
    };
    let check = match tokio::time::timeout(CHECK_TIMEOUT, updater.check()).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current: Some(current),
                latest: None,
                message: format!("更新失败：{e}"),
                ..Default::default()
            };
            notify_app(&app, "应用更新失败", "App update failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
        Err(_) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: false,
                current: Some(current),
                latest: None,
                message: "检查更新超时（已等待 60 秒），请检查网络后重试。".into(),
                ..Default::default()
            };
            notify_app(&app, "应用更新失败", "App update failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
    };
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
    state.app_cancel.store(false, Ordering::SeqCst);
    set_app_stage(&app, UpdateStage::Downloading, true);
    state.set_app_update(DshUpdateStatus {
        ok: true,
        update_available: true,
        current: Some(current.clone()),
        latest: Some(latest.clone()),
        message: format!("正在下载更新 {}", latest),
        ..Default::default()
    });

    let mut received: u64 = 0;
    let mut total: Option<u64> = None;
    let progress_app = app.clone();
    let cancel_app = app.clone();
    let dl_fut = update.download(
        |chunk, content_length| {
            received += chunk as u64;
            if content_length.is_some() {
                total = content_length;
            }
            progress_app.state::<AppState>().set_app_progress(UpdateProgress {
                stage: UpdateStage::Downloading,
                can_cancel: true,
                received: Some(received),
                total,
            });
        },
        || {},
    );
    tokio::pin!(dl_fut);
    let cancel_fut = wait_cancelled(cancel_app);
    tokio::pin!(cancel_fut);
    let download_res = tokio::time::timeout(DOWNLOAD_TIMEOUT, async {
        tokio::select! {
            r = dl_fut => r.map_err(|e| e.to_string()),
            _ = cancel_fut => Err(CANCELLED.to_string()),
        }
    })
    .await;

    let downloaded = match download_res {
        Err(_) => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: true,
                current: Some(current),
                latest: Some(latest),
                message: "下载更新超时（已等待 20 分钟），请检查网络后重试。".into(),
                ..Default::default()
            };
            notify_app(&app, "应用更新失败", "App update failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
        Ok(r) => r,
    };

    let bytes = match downloaded {
        Err(e) if e == CANCELLED => {
            let status = DshUpdateStatus {
                ok: false,
                update_available: true,
                current: Some(current),
                latest: Some(latest),
                message: "已取消，可稍后重试。".into(),
                ..Default::default()
            };
            state.set_app_update(status.clone());
            return Ok(status);
        }
        Err(e) => {
            log::warn!("updater download failed: {e}");
            let status = DshUpdateStatus {
                ok: false,
                update_available: true,
                current: Some(current),
                latest: Some(latest),
                message: format!("更新失败：{e}"),
                ..Default::default()
            };
            notify_app(&app, "应用更新失败", "App update failed", &status.message);
            state.set_app_update(status.clone());
            return Ok(status);
        }
        Ok(bytes) => bytes,
    };

    if crate::dsh_update::dsh_update_active() {
        let status = DshUpdateStatus {
            ok: false,
            update_available: true,
            current: Some(current.clone()),
            latest: Some(latest.clone()),
            message: "DSH 服务正在更新，无法同时安装应用，请稍后重试。".into(),
            ..Default::default()
        };
        state.set_app_update(status.clone());
        return Ok(status);
    }

    state.app_cancel.store(false, Ordering::SeqCst);
    set_app_stage(&app, UpdateStage::Installing, false);
    state.set_app_update(DshUpdateStatus {
        ok: true,
        update_available: true,
        current: Some(current.clone()),
        latest: Some(latest.clone()),
        message: "正在安装更新，应用将自动重启…".into(),
        ..Default::default()
    });
    log::info!("updater: installing {latest}");
    notify_app(&app, "正在安装应用更新", "Installing app update", "正在安装更新，应用将自动重启…");
    tokio::time::sleep(Duration::from_millis(500)).await;
    if let Err(e) = update.install(&bytes) {
        let status = DshUpdateStatus {
            ok: false,
            update_available: true,
            current: Some(current),
            latest: Some(latest),
            message: format!("更新失败：{e}"),
            ..Default::default()
        };
        notify_app(&app, "应用更新失败", "App update failed", &status.message);
        state.set_app_update(status.clone());
        return Ok(status);
    }
    let status = DshUpdateStatus {
        ok: true,
        update_available: false,
        current: Some(latest.clone()),
        latest: Some(latest),
        message: "已安装，重启应用后生效".into(),
        ..Default::default()
    };
    notify_app(&app, "应用更新完成", "App update completed", &status.message);
    state.set_app_update(status.clone());
    Ok(status)
}

#[tauri::command]
pub(crate) fn cancel_app_update(state: State<'_, AppState>) {
    let p = state.app_progress();
    if p.can_cancel && p.stage == UpdateStage::Downloading {
        state.app_cancel.store(true, Ordering::SeqCst);
    }
}
