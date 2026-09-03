use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU8, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::config::ConfigStore;
use crate::supervisor::Supervisor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootPhase {
    NodeCheck,
    DshInstall,
    ServiceStart,
    Ready,
    Error,
}

pub struct AppState {
    pub node_path: Mutex<PathBuf>,
    pub app_data_dir: Mutex<PathBuf>,
    pub runtime_dir: Mutex<PathBuf>,
    pub workspace_dir: Mutex<PathBuf>,
    pub log_file: Mutex<PathBuf>,
    pub port: AtomicU16,
    pub phase: AtomicU8,
    pub error_message: Mutex<Option<String>>,
    pub supervisor: Mutex<Supervisor>,
    pub config: Mutex<ConfigStore>,
    pub dsh_extra_args: Mutex<Vec<String>>,
    pub dsh_update: Mutex<Option<DshUpdateStatus>>,
    pub app_update: Mutex<Option<DshUpdateStatus>>,
    pub boot_page_url: Mutex<Option<String>>,
    pub service_watch_active: AtomicBool,
    pub last_recovery: Mutex<Option<RecoveryInfo>>,
    pub pending_auth_url: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryInfo {
    pub kind: String,
    pub plugins: Vec<String>,
    pub lock_path: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DshUpdateStatus {
    pub ok: bool,
    pub update_available: bool,
    pub current: Option<String>,
    pub latest: Option<String>,
    pub prerelease: Option<String>,
    pub pre_available: bool,
    pub message: String,
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

    pub fn set_dsh_update(&self, status: DshUpdateStatus) {
        *self.dsh_update.lock().unwrap() = Some(status);
    }

    pub fn dsh_update(&self) -> Option<DshUpdateStatus> {
        self.dsh_update.lock().unwrap().clone()
    }

    pub fn set_app_update(&self, status: DshUpdateStatus) {
        *self.app_update.lock().unwrap() = Some(status);
    }

    pub fn app_update(&self) -> Option<DshUpdateStatus> {
        self.app_update.lock().unwrap().clone()
    }

    pub fn set_boot_page_url(&self, url: String) {
        *self.boot_page_url.lock().unwrap() = Some(url);
    }

    pub fn boot_page_url(&self) -> Option<String> {
        self.boot_page_url.lock().unwrap().clone()
    }

    pub fn set_last_recovery(&self, info: RecoveryInfo) {
        *self.last_recovery.lock().unwrap() = Some(info);
    }

    pub fn last_recovery(&self) -> Option<RecoveryInfo> {
        self.last_recovery.lock().unwrap().clone()
    }

    pub fn clear_recovery(&self) {
        *self.last_recovery.lock().unwrap() = None;
    }

    pub fn set_pending_auth_url(&self, url: Option<String>) {
        *self.pending_auth_url.lock().unwrap() = url;
    }

    pub fn pending_auth_url(&self) -> Option<String> {
        self.pending_auth_url.lock().unwrap().clone()
    }
}

#[derive(Debug, Serialize)]
pub struct StatusSnapshot {
    pub phase: BootPhase,
    pub message: String,
    pub error: Option<String>,
    pub port: u16,
    pub service_url: Option<String>,
    pub recovery: Option<RecoveryInfo>,
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
            dsh_update: Mutex::new(None),
            app_update: Mutex::new(None),
            boot_page_url: Mutex::new(None),
            service_watch_active: AtomicBool::new(false),
            last_recovery: Mutex::new(None),
            pending_auth_url: Mutex::new(None),
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
