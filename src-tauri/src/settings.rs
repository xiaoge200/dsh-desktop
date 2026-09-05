use crate::config;
use crate::node;
use crate::state::{AppState, BootPhase};
use tauri::{AppHandle, State};

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
pub(crate) fn get_settings(app: AppHandle, state: State<'_, AppState>) -> Result<SettingsData, String> {
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
