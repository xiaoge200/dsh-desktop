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
    /// 更新源（"auto" 自动探测 / "npmjs" 官方 / "npmmirror" 国内镜像）
    pub registry_source: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_update_dsh: true,
            auto_update_app: true,
            port: 3080,
            registry_source: "auto".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_optimal_for_ordinary_users() {
        let c = AppConfig::default();
        assert!(c.auto_update_dsh, "默认自动更新 DSH");
        assert!(c.auto_update_app, "默认自动更新应用壳");
        assert_eq!(c.port, 3080, "默认端口 3080");
    }

    #[test]
    fn missing_config_file_returns_default() {
        let dir = std::env::temp_dir().join(format!("dsh-cfg-none-{}", std::process::id()));
        let store = ConfigStore::new(&dir);
        assert_eq!(store.get().port, 3080);
        assert!(store.get().auto_update_dsh);
    }

    #[test]
    fn set_and_get_roundtrip() {
        let dir = std::env::temp_dir().join(format!("dsh-cfg-rw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ConfigStore::new(&dir);
        let mut c = store.get();
        c.auto_update_dsh = false;
        c.port = 4000;
        store.set(c.clone()).unwrap();
        // 重新加载（模拟重启后读取）
        let reloaded = ConfigStore::new(&dir);
        assert_eq!(reloaded.get().auto_update_dsh, false);
        assert_eq!(reloaded.get().port, 4000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_config_file_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!("dsh-cfg-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "{corrupt!!").unwrap();
        let store = ConfigStore::new(&dir);
        assert_eq!(store.get().port, 3080, "损坏配置回退默认");
        std::fs::remove_dir_all(&dir).ok();
    }
}
