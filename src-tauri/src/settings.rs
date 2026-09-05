use crate::config;
use crate::state::{AppState, BootPhase};
use tauri::{AppHandle, Manager, State};

#[derive(serde::Serialize)]
pub(crate) struct SettingsData {
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
pub(crate) async fn get_settings(app: AppHandle) -> Result<SettingsData, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_autostart::ManagerExt;

        let pkg = app2.package_info();
        let autostart = app2
            .autolaunch()
            .is_enabled()
            .map_err(|e| format!("读取开机启动设置失败: {e}"))?;
        let state = app2.state::<AppState>();
        let port = state.port();
        let service_running = port > 0 && state.service_health();

        Ok(SettingsData {
            app_version: pkg.version.to_string(),
            node_version: state.cached_node_version(),
            dsh_version: crate::node::read_installed_version(&state.runtime_dir()),
            port,
            service_running,
            phase: state.phase(),
            error: state.error(),
            workspace_dir: state.workspace_dir().display().to_string(),
            log_file: state.log_file().display().to_string(),
            autostart_enabled: autostart,
        })
    })
    .await
    .map_err(|e| format!("读取设置任务异常: {e}"))?
}

#[derive(serde::Serialize)]
pub(crate) struct ServiceState {
    port: u16,
    service_running: bool,
    phase: BootPhase,
    error: Option<String>,
}

#[tauri::command]
pub(crate) async fn get_service_state(app: AppHandle) -> ServiceState {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let port = state.port();
        ServiceState {
            port,
            service_running: port > 0 && state.service_health(),
            phase: state.phase(),
            error: state.error(),
        }
    })
    .await
    .unwrap_or_else(|e| {
        log::warn!("get_service_state task failed: {e}");
        ServiceState {
            port: 0,
            service_running: false,
            phase: BootPhase::Error,
            error: Some("状态读取失败".into()),
        }
    })
}

#[tauri::command]
pub(crate) fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
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
pub(crate) fn open_log_dir(state: State<'_, AppState>) -> Result<(), String> {
    let log_file = state.log_file();
    let dir = log_file
        .parent()
        .ok_or("无法确定日志位置")?;
    open::that(dir).map_err(|e| format!("打开日志目录失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub(crate) fn open_workspace_dir(state: State<'_, AppState>) -> Result<(), String> {
    let ws = state.workspace_dir();
    if ws.as_os_str().is_empty() {
        return Err("工作区尚未初始化".into());
    }
    open::that(&ws).map_err(|e| format!("打开工作区失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub(crate) fn get_config(state: State<'_, AppState>) -> Result<config::AppConfig, String> {
    Ok(state.config.lock().unwrap().get())
}

#[tauri::command]
pub(crate) fn set_config(state: State<'_, AppState>, config: config::AppConfig) -> Result<(), String> {
    state.config.lock().unwrap().set(config)
}
