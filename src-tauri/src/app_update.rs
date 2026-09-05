use crate::state::{AppState, DshUpdateStatus};
use tauri::{AppHandle, State};

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
