use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// 极简滚动日志：追加写文件，单行文本。日志文件路径在启动时设置。
pub struct FileLogger {
    file: Mutex<Option<File>>,
    max_bytes: u64,
}

impl FileLogger {
    pub fn new() -> Self {
        Self { file: Mutex::new(None), max_bytes: 2 * 1024 * 1024 }
    }

    /// 设置日志文件；若文件超过 max_bytes 则截断（首版简化为重命名 .old）
    pub fn init(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 简单轮转：超过上限就改名 .old
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > self.max_bytes {
                let old = path.with_extension("log.old");
                let _ = std::fs::rename(path, old);
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        *self.file.lock().unwrap() = Some(file);
        Ok(())
    }

    pub fn write_line(&self, line: &str) {
        let mut guard = self.file.lock().unwrap();
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{}", line);
            let _ = f.flush();
        }
        // 同时输出到 stderr（dev 可见）
        eprintln!("{}", line);
    }
}

/// 全局 logger（log crate 的 facade 目标）
pub struct TauriLogBridge(pub std::sync::Arc<FileLogger>);

impl log::Log for TauriLogBridge {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let line = format!(
            "[{}] {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        self.0.write_line(&line);
    }

    fn flush(&self) {}
}
