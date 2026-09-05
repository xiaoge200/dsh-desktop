use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::serviceout::ServiceCapture;

#[cfg(windows)]
type SpawnedChild = crate::winproc::ChildHandle;
#[cfg(not(windows))]
type SpawnedChild = std::process::Child;

#[cfg(windows)]
mod win {
    use std::mem::size_of;

    use winapi::shared::minwindef::{FALSE, LPVOID};
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::jobapi2::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
    };
    use winapi::um::processthreadsapi::{OpenThread, ResumeThread};
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use winapi::um::winnt::{
        JobObjectExtendedLimitInformation, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, THREAD_SUSPEND_RESUME, HANDLE,
    };

    pub struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn create_kill_on_close_job() -> Option<Job> {
        unsafe {
            let h = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as LPVOID,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(h);
                return None;
            }
            Some(Job(h))
        }
    }

    pub fn assign(job: &Job, child_handle: HANDLE) -> bool {
        unsafe { AssignProcessToJobObject(job.0, child_handle) != 0 }
    }

    pub fn resume_threads(pid: u32) {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return;
            }
            let mut te: THREADENTRY32 = std::mem::zeroed();
            te.dwSize = size_of::<THREADENTRY32>() as u32;
            if Thread32First(snap, &mut te) == 0 {
                CloseHandle(snap);
                return;
            }
            loop {
                if te.th32OwnerProcessID == pid {
                    let h = OpenThread(THREAD_SUSPEND_RESUME, FALSE, te.th32ThreadID);
                    if !h.is_null() {
                        ResumeThread(h);
                        CloseHandle(h);
                    }
                }
                if Thread32Next(snap, &mut te) == 0 {
                    break;
                }
            }
            CloseHandle(snap);
        }
    }

    pub fn terminate(job: &Job) {
        unsafe { TerminateJobObject(job.0, 1) };
    }
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

pub fn probe_port(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    match TcpStream::connect_timeout(&loopback(port), HEALTH_TIMEOUT) {
        Ok(mut stream) => {
            let req = format!(
                "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nUser-Agent: dsh-desktop/0.1\r\n\r\n"
            );
            let _ = stream.set_read_timeout(Some(HEALTH_TIMEOUT));
            if stream.write_all(req.as_bytes()).is_err() {
                return false;
            }
            let mut buf = [0u8; 128];
            match stream.read(&mut buf) {
                Ok(_) => {
                    let head = String::from_utf8_lossy(&buf);
                    head.starts_with("HTTP/1") || head.contains("HTTP/1")
                }
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

const STALE_CMD_MARKER: &str = "dsh-market-restart";

fn runtime_dir_from_bin(bin: &Path) -> Option<PathBuf> {
    let mut prev: Option<&std::ffi::OsStr> = None;
    for anc in bin.ancestors().skip(1) {
        let name = anc.file_name()?;
        if prev == Some(std::ffi::OsStr::new("@deepseek-ai"))
            && name == std::ffi::OsStr::new("node_modules")
        {
            return Some(anc.parent()?.to_path_buf());
        }
        prev = Some(name);
    }
    None
}

fn cmd_matches_markers(cmd: &[String], markers: &[String]) -> bool {
    let hay: String = cmd.iter().map(|a| a.to_lowercase()).collect::<Vec<_>>().join(" ");
    markers.iter().any(|m| hay.contains(&m.to_lowercase()))
}

fn normalize_for_node(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        let stripped = s
            .strip_prefix(r"\\?\")
            .or_else(|| s.strip_prefix(r"\??\"))
            .unwrap_or(&s);
        PathBuf::from(stripped.to_string())
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

pub struct Supervisor {
    child: Mutex<Option<SpawnedChild>>,
    port: AtomicU32,
    restarts: Mutex<Vec<Instant>>,
    degraded: Mutex<bool>,
    log_dir: Mutex<Option<PathBuf>>,
    capture: Mutex<Option<Arc<ServiceCapture>>>,
    reader_done: Mutex<Option<(mpsc::Receiver<()>, mpsc::Receiver<()>)>>,
    dsh_bin: Mutex<Option<PathBuf>>,
    #[cfg(windows)]
    job: Mutex<Option<win::Job>>,
}

const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(300);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(300);

impl Supervisor {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            port: AtomicU32::new(0),
            restarts: Mutex::new(Vec::new()),
            degraded: Mutex::new(false),
            log_dir: Mutex::new(None),
            capture: Mutex::new(None),
            reader_done: Mutex::new(None),
            dsh_bin: Mutex::new(None),
            #[cfg(windows)]
            job: Mutex::new(None),
        }
    }

    pub fn set_log_dir(&self, dir: PathBuf) {
        *self.log_dir.lock().unwrap() = Some(dir);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst) as u16
    }

    pub fn is_degraded(&self) -> bool {
        *self.degraded.lock().unwrap()
    }

    pub fn pick_free_port(preferred: u16) -> u16 {
        if TcpStream::connect(loopback(preferred)).is_err() {
            return preferred;
        }
        match std::net::TcpListener::bind(loopback(0)) {
            Ok(l) => match l.local_addr() {
                Ok(addr) => addr.port(),
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    }

    pub fn start(
        &mut self,
        node: &Path,
        dsh_bin: &Path,
        workspace: &Path,
        preferred_port: u16,
        extra_args: &[String],
    ) -> Result<u16, String> {
        *self.dsh_bin.lock().unwrap() = Some(dsh_bin.to_path_buf());
        self.stop();

        let port = Self::pick_free_port(preferred_port);
        if port == 0 {
            return Err("无法确定可用端口".into());
        }

        let mut sink: Option<(std::fs::File, std::fs::File)> = None;
        if let Some(dir) = self.log_dir.lock().unwrap().clone() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                log::warn!("create service log dir: {e}");
            }
            let log_path = dir.join("service.log");
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&log_path);
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .write(true)
                .open(&log_path)
            {
                Ok(f) => match f.try_clone() {
                    Ok(g) => sink = Some((f, g)),
                    Err(e) => log::warn!("clone service log handle: {e}"),
                },
                Err(e) => log::warn!("open service log: {e}"),
            }
        }
        let capture: Arc<ServiceCapture> =
            Arc::new(ServiceCapture::new(crate::serviceout::DEFAULT_LIMIT));
        *self.capture.lock().unwrap() = None;
        *self.reader_done.lock().unwrap() = None;

        if let Err(e) = std::fs::create_dir_all(workspace) {
            log::warn!("ensure workspace dir: {e}");
        }

        let mut args: Vec<String> = vec![
            normalize_for_node(dsh_bin).to_string_lossy().to_string(),
            "web".to_string(),
            "--no-open".to_string(),
            "--port".to_string(),
            port.to_string(),
        ];
        for a in extra_args {
            args.push(a.clone());
        }

        #[cfg(windows)]
        let child: SpawnedChild = {
            use crate::winproc::{create_pipe, spawn_hidden, SpawnOpts};
            let pipe_res: Result<(crate::winproc::ChildHandle, std::fs::File, std::fs::File), String> =
                (|| {
                    let (out_r, out_w) =
                        create_pipe().map_err(|e| format!("create stdout pipe: {e}"))?;
                    let (err_r, err_w) =
                        create_pipe().map_err(|e| format!("create stderr pipe: {e}"))?;
                    let c = spawn_hidden(SpawnOpts {
                        program: normalize_for_node(node),
                        args: args.clone(),
                        cwd: Some(normalize_for_node(workspace)),
                        stdout: Some(out_w),
                        stderr: Some(err_w),
                        suspend: true,
                        process_group: true,
                    })
                    .map_err(|e| format!("启动服务失败: {e}"))?;
                    Ok((c, out_r, err_r))
                })();
            match pipe_res {
                Ok((child, out_r, err_r)) => {
                    log::info!("service spawned (hidden console, pipe capture), pid={}", child.id());

                    let job = win::create_kill_on_close_job();
                    if let Some(ref j) = job {
                        if win::assign(j, child.as_raw_handle()) {
                            log::info!("service attached to kill-on-close job");
                        } else {
                            log::warn!("assign service to job failed; falling back to taskkill cleanup");
                        }
                    }
                    *self.job.lock().unwrap() = job;

                    self.spawn_readers(out_r, err_r, sink, capture.clone());
                    win::resume_threads(child.id());
                    child
                }
                Err(e) if e.starts_with("启动服务失败") => return Err(e),
                Err(e) => {
                    log::warn!("{e}; falling back to direct log redirect");
                    let stdout_f = sink.as_ref().and_then(|(a, _)| a.try_clone().ok());
                    let stderr_f = sink.as_ref().and_then(|(_, b)| b.try_clone().ok());
                    if stdout_f.is_none() || stderr_f.is_none() {
                        log::warn!("open service log (fallback redirect) failed");
                    }
                    let child = spawn_hidden(SpawnOpts {
                        program: normalize_for_node(node),
                        args: args.clone(),
                        cwd: Some(normalize_for_node(workspace)),
                        stdout: stdout_f,
                        stderr: stderr_f,
                        suspend: true,
                        process_group: true,
                    })
                    .map_err(|e| format!("启动服务失败: {e}"))?;
                    log::info!("service spawned (hidden console, direct log), pid={}", child.id());
                    let job = win::create_kill_on_close_job();
                    if let Some(ref j) = job {
                        if win::assign(j, child.as_raw_handle()) {
                            log::info!("service attached to kill-on-close job");
                        } else {
                            log::warn!("assign service to job failed; falling back to taskkill cleanup");
                        }
                    }
                    *self.job.lock().unwrap() = job;
                    win::resume_threads(child.id());
                    child
                }
            }
        };

        #[cfg(unix)]
        let child: SpawnedChild = {
            let mut cmd = Command::new(normalize_for_node(node));
            cmd.args(&args)
                .current_dir(normalize_for_node(workspace))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
            log::info!("spawn: {:?}", cmd);
            let mut child = cmd.spawn().map_err(|e| format!("启动服务失败: {e}"))?;
            log::info!("service spawned, pid={}", child.id());
            match (child.stdout.take(), child.stderr.take()) {
                (Some(out), Some(err)) => self.spawn_readers(out, err, sink, capture.clone()),
                _ => log::warn!("service pipes unavailable; capture disabled"),
            }
            child
        };

        *self.child.lock().unwrap() = Some(child);
        self.port.store(port as u32, Ordering::SeqCst);
        log::info!("service spawning on 127.0.0.1:{port}");
        Ok(port)
    }


    pub fn is_exited(&self) -> bool {
        let mut guard = self.child.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    *guard = None;
                    true
                }
                Ok(None) => false,
                Err(_) => true,
            }
        } else {
            true
        }
    }

    pub fn captured_url(&self) -> Option<String> {
        let cap = self.capture.lock().unwrap().clone()?;
        let port = self.port();
        if port == 0 {
            return None;
        }
        cap.captured_url(port)
    }

    pub fn failure_classify(&self) -> Option<crate::serviceout::FailureKind> {
        let cap = self.capture.lock().unwrap().clone()?;
        Some(cap.classify())
    }

    pub fn output_tail(&self, max: usize) -> String {
        let cap = match self.capture.lock().unwrap().clone() {
            Some(c) => c,
            None => return String::new(),
        };
        let text = cap.text();
        if text.len() <= max {
            return text;
        }
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_string()
    }

    fn spawn_readers<R1: std::io::Read + Send + 'static, R2: std::io::Read + Send + 'static>(
        &self,
        out_r: R1,
        err_r: R2,
        sink: Option<(std::fs::File, std::fs::File)>,
        cap: Arc<ServiceCapture>,
    ) {
        *self.capture.lock().unwrap() = Some(cap.clone());
        let (tx_out, rx_out) = mpsc::channel();
        let (tx_err, rx_err) = mpsc::channel();
        let (sink_out, sink_err) = sink.map(|(a, b)| (Some(a), Some(b))).unwrap_or((None, None));
        let cap_out = cap.clone();
        let cap_err = cap.clone();
        let t_out = std::thread::Builder::new()
            .name("service-out".into())
            .spawn(move || reader_loop(out_r, sink_out, cap_out, tx_out));
        let t_err = std::thread::Builder::new()
            .name("service-err".into())
            .spawn(move || reader_loop(err_r, sink_err, cap_err, tx_err));
        if t_out.is_err() {
            log::warn!("spawn stdout reader thread failed; capture degraded");
        }
        if t_err.is_err() {
            log::warn!("spawn stderr reader thread failed; capture degraded");
        }
        *self.reader_done.lock().unwrap() = Some((rx_out, rx_err));
    }

    fn allow_restart(&self) -> bool {
        let now = Instant::now();
        let mut window = self.restarts.lock().unwrap();
        window.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
        *self.degraded.lock().unwrap() = window.len() >= MAX_RESTARTS;
        if window.len() >= MAX_RESTARTS {
            false
        } else {
            window.push(now);
            true
        }
    }

    pub fn restart(&mut self, node: &Path, dsh_bin: &Path, workspace: &Path, preferred_port: u16, extra_args: &[String]) -> Result<u16, String> {
        if !self.allow_restart() {
            log::warn!("service exceeded restart limit; degraded mode");
            return Err("服务多次启动失败".into());
        }
        log::warn!("restarting service");
        self.start(node, dsh_bin, workspace, preferred_port, extra_args)
    }

    pub fn stop(&mut self) {
        #[cfg(windows)]
        {
            if let Some(j) = self.job.lock().unwrap().take() {
                win::terminate(&j);
            }
        }
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            log::info!("stopping service pid={}", child.id());
            let pid = child.id();
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status();
                let _ = child.kill();
            }
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                        Err(_) => break,
                    }
                }
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.wait();
            log::info!("service stopped");
        }
        self.kill_stale_dsh();

        if let Some((rx_out, rx_err)) = self.reader_done.lock().unwrap().take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            for rx in [rx_out, rx_err] {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() || rx.recv_timeout(remaining).is_err() {
                    log::warn!("service reader thread did not exit in time");
                    break;
                }
            }
        }
    }

    pub fn ensure_stopped(&mut self) {
        self.stop();
        for _ in 0..4 {
            if !self.kill_stale_dsh() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn kill_stale_dsh(&self) -> bool {
        let bin = match self.dsh_bin.lock().unwrap().clone() {
            Some(p) => normalize_for_node(&p),
            None => return false,
        };
        let mut markers = vec![
            bin.to_string_lossy().to_lowercase(),
            STALE_CMD_MARKER.to_string(),
        ];
        if let Some(runtime) = runtime_dir_from_bin(&bin) {
            markers.push(runtime.to_string_lossy().to_lowercase());
        }
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
        );
        let current = sysinfo::get_current_pid().ok();
        let mut killed_any = false;
        for (pid, proc_) in sys.processes() {
            if Some(*pid) == current {
                continue;
            }
            let cmd: Vec<String> = proc_.cmd().iter().map(|a| a.to_string_lossy().to_string()).collect();
            if !cmd_matches_markers(&cmd, &markers) {
                continue;
            }
            killed_any = true;
            log::info!("killing stale dsh process pid={pid}");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.as_u32().to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status();
            }
            #[cfg(not(windows))]
            {
                let _ = proc_.kill();
            }
        }
        killed_any
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn reader_loop<R: std::io::Read>(
    mut r: R,
    mut sink: Option<std::fs::File>,
    cap: Arc<ServiceCapture>,
    done: mpsc::Sender<()>,
) {
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                if let Some(f) = sink.as_mut() {
                    if f.write_all(chunk).is_ok() {
                        let _ = f.flush();
                    }
                }
                cap.push_bytes(chunk);
            }
        }
    }
    let _ = done.send(());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_addr_is_127_0_0_1() {
        let a = loopback(3080);
        assert_eq!(a.ip().to_string(), "127.0.0.1");
        assert_eq!(a.port(), 3080);
    }

    #[test]
    fn normalize_for_node_strips_verbatim_prefix() {
        let verbatim = PathBuf::from(r"\\?\C:\Users\test\dsh");
        let n = normalize_for_node(&verbatim);
        let s = n.to_string_lossy().to_string();
        assert!(!s.starts_with(r"\\?\"));
        assert!(s.contains(r"C:\Users\test\dsh"));
    }

    #[test]
    fn normalize_for_node_keeps_plain_path() {
        let plain = PathBuf::from(r"C:\Users\test\dsh");
        let n = normalize_for_node(&plain);
        assert_eq!(n, plain);
    }

    #[test]
    fn runtime_dir_from_bin_finds_runtime_above_scoped_modules() {
        let bin = PathBuf::from(r"C:\Users\me\AppData\Roaming\com.dsh.desktop\dsh-runtime\node_modules\@deepseek-ai\dsh\lib\bin.js");
        assert_eq!(
            runtime_dir_from_bin(&bin),
            Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\com.dsh.desktop\dsh-runtime"))
        );
    }

    #[test]
    fn runtime_dir_from_bin_returns_none_for_unrelated_paths() {
        assert_eq!(runtime_dir_from_bin(Path::new(r"C:\x\node_modules\plain\bin.js")), None);
        assert_eq!(runtime_dir_from_bin(Path::new(r"C:\x\@deepseek-ai\dsh\bin.js")), None);
    }

    #[test]
    fn cmd_matches_markers_case_insensitive_contains() {
        let markers = vec![
            r"c:\dsh-runtime".to_string(),
            "dsh-market-restart".to_string(),
        ];
        assert!(cmd_matches_markers(
            &["node.exe".into(), r"C:\DSH-RUNTIME\node_modules\@deepseek-ai\dsh\lib\bin.js".into()],
            &markers
        ));
        assert!(cmd_matches_markers(
            &["node.exe".into(), "-e".into(), "const dsh-market-restart = 1".into()],
            &markers
        ));
        assert!(!cmd_matches_markers(
            &["powershell.exe".into(), "-Command".into(), "echo hi".into()],
            &markers
        ));
    }

    #[test]
    fn pick_free_port_returns_preferred_when_free() {
        let port = Supervisor::pick_free_port(3080);
        assert!(port > 0);
    }

    #[test]
    fn pick_free_port_returns_alternative_when_occupied() {
        let listener = std::net::TcpListener::bind(loopback(0)).unwrap();
        let occupied = listener.local_addr().unwrap().port();
        let picked = Supervisor::pick_free_port(occupied);
        assert!(picked > 0);
        assert_ne!(picked, occupied);
    }

    #[test]
    fn health_check_fails_on_closed_port() {
        let listener = std::net::TcpListener::bind(loopback(0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let sup = Supervisor::new();
        sup.port.store(port as u32, Ordering::SeqCst);
        assert!(!probe_port(sup.port()));
    }

    #[test]
    fn health_check_succeeds_on_http_server() {
        let listener = std::net::TcpListener::bind(loopback(0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            }
        });
        let sup = Supervisor::new();
        sup.port.store(port as u32, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if probe_port(sup.port()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("health_check should succeed against a live HTTP server");
    }
}
