use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::config::ConfigStore;
use crate::supervisor::Supervisor;

/// 启动阶段（与前端 boot UI 对应，白话文案由前端映射）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootPhase {
    /// 校验内置 Node
    NodeCheck,
    /// 安装/更新 DSH 运行时
    DshInstall,
    /// 启动本地服务
    ServiceStart,
    /// 服务就绪，WebView 装载
    Ready,
    /// 失败（error 字段带白话信息）
    Error,
}

/// 壳的全局状态
pub struct AppState {
    /// 内置 Node 二进制绝对路径
    pub node_path: Mutex<PathBuf>,
    /// 应用数据目录（appData，config.json / 插件快照等所在）
    pub app_data_dir: Mutex<PathBuf>,
    /// DSH 运行时根目录（appData/dsh-runtime）
    pub runtime_dir: Mutex<PathBuf>,
    /// 工作区目录（dsh 的 cwd）
    pub workspace_dir: Mutex<PathBuf>,
    /// 日志文件路径
    pub log_file: Mutex<PathBuf>,
    /// 当前监听端口（3080 或自动更换后的空闲端口）
    pub port: AtomicU16,
    /// 启动阶段
    pub phase: AtomicU8,
    /// 白话错误信息（phase == Error 时有效）
    pub error_message: Mutex<Option<String>>,
    /// 服务进程托管
    pub supervisor: Mutex<Supervisor>,
    /// 用户配置（设置页）
    pub config: Mutex<ConfigStore>,
    /// 高级用户透传的 dsh CLI 参数（FR-15，`--dsh-args` 启动参数）
    pub dsh_extra_args: Mutex<Vec<String>>,
}

impl AppState {
    pub fn set_phase(&self, phase: BootPhase) {
        self.phase.store(phase as u8, Ordering::SeqCst);
    }

    pub fn phase(&self) -> BootPhase {
        match self.phase.load(Ordering::SeqCst) {
            0 => BootPhase::NodeCheck,
            1 => BootPhase::DshInstall,
            2 => BootPhase::ServiceStart,
            3 => BootPhase::Ready,
            _ => BootPhase::Error,
        }
    }

    pub fn set_error(&self, message: impl Into<String>) {
        *self.error_message.lock().unwrap() = Some(message.into());
        self.set_phase(BootPhase::Error);
    }

    pub fn error(&self) -> Option<String> {
        self.error_message.lock().unwrap().clone()
    }

    pub fn clear_error(&self) {
        *self.error_message.lock().unwrap() = None;
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    pub fn set_node_path(&self, p: PathBuf) {
        *self.node_path.lock().unwrap() = p;
    }

    pub fn node_path(&self) -> PathBuf {
        self.node_path.lock().unwrap().clone()
    }

    pub fn set_app_data_dir(&self, p: PathBuf) {
        *self.app_data_dir.lock().unwrap() = p;
    }

    pub fn app_data_dir(&self) -> PathBuf {
        self.app_data_dir.lock().unwrap().clone()
    }

    pub fn set_runtime_dir(&self, p: PathBuf) {
        *self.runtime_dir.lock().unwrap() = p;
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.runtime_dir.lock().unwrap().clone()
    }

    pub fn set_workspace_dir(&self, p: PathBuf) {
        *self.workspace_dir.lock().unwrap() = p;
    }

    pub fn workspace_dir(&self) -> PathBuf {
        self.workspace_dir.lock().unwrap().clone()
    }

    pub fn set_log_file(&self, p: PathBuf) {
        *self.log_file.lock().unwrap() = p;
    }

    pub fn log_file(&self) -> PathBuf {
        self.log_file.lock().unwrap().clone()
    }
}

/// 前端读取的状态快照
#[derive(Debug, Serialize)]
pub struct StatusSnapshot {
    pub phase: BootPhase,
    pub message: String,
    pub error: Option<String>,
    pub port: u16,
    pub dsh_version: Option<String>,
    pub node_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_state() -> AppState {
        AppState {
            node_path: Mutex::new(PathBuf::new()),
            app_data_dir: Mutex::new(PathBuf::new()),
            runtime_dir: Mutex::new(PathBuf::new()),
            workspace_dir: Mutex::new(PathBuf::new()),
            log_file: Mutex::new(PathBuf::new()),
            port: AtomicU16::new(0),
            phase: AtomicU8::new(0),
            error_message: Mutex::new(None),
            supervisor: Mutex::new(Supervisor::new()),
            config: Mutex::new(ConfigStore::new(std::path::Path::new(""))),
            dsh_extra_args: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn phase_roundtrip() {
        let s = new_state();
        for (idx, p) in [
            BootPhase::NodeCheck,
            BootPhase::DshInstall,
            BootPhase::ServiceStart,
            BootPhase::Ready,
            BootPhase::Error,
        ]
        .iter()
        .enumerate()
        {
            s.set_phase(*p);
            assert_eq!(s.phase(), *p, "phase #{idx}");
        }
    }

    #[test]
    fn error_sets_phase_to_error() {
        let s = new_state();
        s.set_error("程序出了问题");
        assert_eq!(s.phase(), BootPhase::Error);
        assert_eq!(s.error().as_deref(), Some("程序出了问题"));
        s.clear_error();
        assert_eq!(s.error(), None);
    }

    #[test]
    fn port_roundtrip() {
        let s = new_state();
        s.set_port(0);
        assert_eq!(s.port(), 0);
        s.set_port(3080);
        assert_eq!(s.port(), 3080);
        s.set_port(65535);
        assert_eq!(s.port(), 65535);
    }

    #[test]
    fn path_accessors_roundtrip() {
        let s = new_state();
        s.set_node_path(PathBuf::from("/tmp/node"));
        s.set_runtime_dir(PathBuf::from("/tmp/runtime"));
        s.set_workspace_dir(PathBuf::from("/tmp/ws"));
        s.set_log_file(PathBuf::from("/tmp/app.log"));
        assert_eq!(s.node_path(), PathBuf::from("/tmp/node"));
        assert_eq!(s.runtime_dir(), PathBuf::from("/tmp/runtime"));
        assert_eq!(s.workspace_dir(), PathBuf::from("/tmp/ws"));
        assert_eq!(s.log_file(), PathBuf::from("/tmp/app.log"));
    }
}
