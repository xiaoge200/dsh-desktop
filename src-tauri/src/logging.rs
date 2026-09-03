use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

pub struct FileLogger {
    file: Mutex<Option<File>>,
    max_bytes: u64,
}

impl FileLogger {
    pub fn new() -> Self {
        Self { file: Mutex::new(None), max_bytes: 2 * 1024 * 1024 }
    }

    pub fn init(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

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

        eprintln!("{}", line);
    }
}

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
