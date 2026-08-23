use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 用户可配置项（FR-14：设置页）。
/// 存为 appData/config.json，默认值即 0 门槛最优解。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 是否自动更新 DSH 包（默认 true）
    pub auto_update_dsh: bool,
    /// 是否自动更新应用壳（默认 true）
    pub auto_update_app: bool,
    /// 自定义端口（0 = 自动，默认 3080 优先）
    pub port: u16,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_update_dsh: true,
            auto_update_app: true,
            port: 3080,
        }
    }
}

/// 配置文件读写（轻量，无外部依赖）
pub struct ConfigStore {
    path: PathBuf,
    inner: Mutex<AppConfig>,
}

impl ConfigStore {
    pub fn new(app_data: &std::path::Path) -> Self {
        let path = app_data.join("config.json");
        let config = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
            .unwrap_or_default();
        Self { path, inner: Mutex::new(config) }
    }

    pub fn get(&self) -> AppConfig {
        self.inner.lock().unwrap().clone()
    }

    pub fn set(&self, config: AppConfig) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
        fs::write(&self.path, json).map_err(|e| format!("保存设置失败: {e}"))?;
        *self.inner.lock().unwrap() = config;
        Ok(())
    }
}
