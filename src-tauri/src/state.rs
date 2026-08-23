use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use std::sync::Mutex;

use serde::Serialize;

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
