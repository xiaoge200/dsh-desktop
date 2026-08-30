use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 127.0.0.1 的 SocketAddr（端口动态）
fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// 去掉 Windows verbatim 路径前缀（`\\?\`），Node 24 无法以这种路径作为入口脚本
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

/// 服务进程托管：spawn `dsh web --no-open`、健康检查、限次重启、退出清理。
///
/// 设计对应 dsh-desktop-plan.md §7：
/// - 端口：优先 3080，被占用时自动探测空闲端口（用户无感）
/// - 健康检查：TCP + HTTP GET / 探测
/// - 重启退避：5 分钟内最多 3 次，超出进入降级态
/// - 退出清理：Windows 用 taskkill /T /F，其他平台 kill 进程组
pub struct Supervisor {
    child: Mutex<Option<Child>>,
    /// 服务实际监听端口
    port: AtomicU32,
    /// 5 分钟滑动窗口内的重启计数
    restarts: Mutex<Vec<Instant>>,
    /// 是否已进入降级态（超过重启上限）
    degraded: Mutex<bool>,
    /// 服务日志目录（stdout/stderr 落盘，FR-09）
    log_dir: Mutex<Option<PathBuf>>,
    /// dsh 入口脚本绝对路径（用于按命令行标记清理残留/孤儿服务进程）
    dsh_bin: Mutex<Option<PathBuf>>,
}

const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(300);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(800);

impl Supervisor {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            port: AtomicU32::new(0),
            restarts: Mutex::new(Vec::new()),
            degraded: Mutex::new(false),
            log_dir: Mutex::new(None),
            dsh_bin: Mutex::new(None),
        }
    }

    /// 设置服务日志目录（启动前调用；stdout/stderr 会落盘到 <dir>/service.log）
    pub fn set_log_dir(&self, dir: PathBuf) {
        *self.log_dir.lock().unwrap() = Some(dir);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst) as u16
    }

    /// 是否已进入降级态（超过重启上限）
    #[allow(dead_code)]
    pub fn is_degraded(&self) -> bool {
        *self.degraded.lock().unwrap()
    }

    /// 探测一个空闲端口（默认 3080，被占用则系统分配）
    pub fn pick_free_port(preferred: u16) -> u16 {
        if TcpStream::connect(loopback(preferred)).is_err() {
            return preferred;
        }
        // 被占用：让 OS 分配一个临时端口
        match std::net::TcpListener::bind(loopback(0)) {
            Ok(l) => match l.local_addr() {
                Ok(addr) => addr.port(),
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    }

    /// 启动服务：node <dsh>/lib/bin.js web --no-open --port <port> [extra_args...]
    /// 返回实际使用的端口（0 表示启动失败）。extra_args 为高级用户透传的 dsh 参数（FR-15）。
    pub fn start(
        &mut self,
        node: &Path,
        dsh_bin: &Path,
        workspace: &Path,
        preferred_port: u16,
        extra_args: &[String],
    ) -> Result<u16, String> {
        // 先记录入口脚本，stop() 才能按命令行标记清理残留进程（须在 stop 之前）
        *self.dsh_bin.lock().unwrap() = Some(dsh_bin.to_path_buf());
        self.stop();

        let port = Self::pick_free_port(preferred_port);
        if port == 0 {
            return Err("无法确定可用端口".into());
        }

        // stdout/stderr 落盘（FR-09）：不 pipe——pipe 且不读取会导致缓冲填满后
        // dsh 的 write 永久阻塞（服务卡死）。直接重定向到 service.log（每次启动截断）。
        let mut stdout_file: Option<std::fs::File> = None;
        let mut stderr_file: Option<std::fs::File> = None;
        if let Some(dir) = self.log_dir.lock().unwrap().clone() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                log::warn!("create service log dir: {e}");
            }
            let log_path = dir.join("service.log");
            let open = || std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&log_path);
            match open() {
                Ok(f) => {
                    let _ = f.set_len(0);
                    stdout_file = Some(f);
                    // stderr 复用同一文件：单独再开一个句柄（同为截断写）
                    stderr_file = open().ok();
                }
                Err(e) => log::warn!("open service log: {e}"),
            }
        }

        // 确保工作区目录存在（current_dir 必须存在，否则 spawn 报 os error 267）
        if let Err(e) = std::fs::create_dir_all(workspace) {
            log::warn!("ensure workspace dir: {e}");
        }

        let mut cmd = Command::new(normalize_for_node(node));
        cmd.arg(normalize_for_node(dsh_bin))
            .arg("web")
            .arg("--no-open")
            .arg("--port")
            .arg(port.to_string())
            .current_dir(normalize_for_node(workspace));
        for a in extra_args {
            cmd.arg(a);
        }
        if let Some(f) = stdout_file {
            cmd.stdout(f);
        } else {
            cmd.stdout(Stdio::null());
        }
        if let Some(f) = stderr_file {
            cmd.stderr(f);
        } else {
            cmd.stderr(Stdio::null());
        }

        // 进程组：便于退出时清理整棵进程树
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：不弹黑窗口（node 子进程）；CREATE_NEW_PROCESS_GROUP：便于整组清理
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        }

        log::info!("spawn: {:?}", cmd);
        let child = cmd.spawn().map_err(|e| format!("启动服务失败: {e}"))?;
        *self.child.lock().unwrap() = Some(child);
        self.port.store(port as u32, Ordering::SeqCst);
        log::info!("service spawning on 127.0.0.1:{port}");
        Ok(port)
    }

    /// 健康检查：HTTP GET /，成功返回 true。失败返回 false。
    pub fn health_check(&self) -> bool {
        let port = self.port();
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

    /// 等待服务就绪：轮询健康检查，最多 timeout。
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.health_check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        self.health_check()
    }

    /// 检查子进程是否已退出（返回 true 表示已退出）。
    #[allow(dead_code)]
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

    /// 记录一次崩溃并判断是否允许重启。
    fn allow_restart(&self) -> bool {
        let now = Instant::now();
        let mut window = self.restarts.lock().unwrap();
        window.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
        if window.len() >= MAX_RESTARTS {
            *self.degraded.lock().unwrap() = true;
            false
        } else {
            window.push(now);
            true
        }
    }

    /// 重启服务：先记录崩溃，若未超限则重启。
    pub fn restart(&mut self, node: &Path, dsh_bin: &Path, workspace: &Path, preferred_port: u16, extra_args: &[String]) -> Result<u16, String> {
        if !self.allow_restart() {
            log::warn!("service exceeded restart limit; degraded mode");
            return Err("服务多次启动失败".into());
        }
        log::warn!("restarting service");
        self.start(node, dsh_bin, workspace, preferred_port, extra_args)
    }

    /// 停止服务并清理进程树。
    pub fn stop(&mut self) {
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            log::info!("stopping service pid={}", child.id());
            let pid = child.id();
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // taskkill /T /F 杀整棵树（CREATE_NO_WINDOW：避免闪黑窗口）
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
                // 向进程组发 SIGTERM，最多等 5s，超时 SIGKILL 兜底（避免 Drop 阻塞）
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
        // 清理残留的 dsh 进程（上一会话异常退出遗留的孤儿进程、taskkill /T
        // 未覆盖的游离进程）。它们占着端口/运行时，会导致新服务启动失败。
        self.kill_stale_dsh();
    }

    /// 按命令行标记清理残留的 dsh 服务进程。
    ///
    /// 只匹配命令行里含本应用 dsh 入口脚本（bin.js 绝对路径）的 node 进程，
    /// 不碰安装器（install-dsh.mjs）、其他程序的 node 进程。
    /// Windows 上对每个命中进程再 taskkill /T，连其派生的子进程一起杀。
    fn kill_stale_dsh(&self) {
        let marker = match self.dsh_bin.lock().unwrap().clone() {
            Some(p) => normalize_for_node(&p).to_string_lossy().to_lowercase(),
            None => return,
        };
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            // 只需命令行，避免 everything() 额外枚举环境变量等开销
            sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
        );
        let current = sysinfo::get_current_pid().ok();
        for (pid, proc_) in sys.processes() {
            if Some(*pid) == current {
                continue;
            }
            let matches = proc_
                .cmd()
                .iter()
                .any(|a| a.to_string_lossy().to_lowercase().contains(&marker));
            if !matches {
                continue;
            }
            log::info!("killing stale dsh process pid={pid}");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // CREATE_NO_WINDOW：避免杀进程时闪黑窗口
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
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
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
        // `\\?\C:\path` → `C:\path`
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
    fn pick_free_port_returns_preferred_when_free() {
        // 3080 通常未被占用；若占用则返回别的端口（也是有效值）
        let port = Supervisor::pick_free_port(3080);
        assert!(port > 0);
    }

    #[test]
    fn pick_free_port_returns_alternative_when_occupied() {
        // 占用一个端口后，pick_free_port 应返回另一个空闲端口
        let listener = std::net::TcpListener::bind(loopback(0)).unwrap();
        let occupied = listener.local_addr().unwrap().port();
        let picked = Supervisor::pick_free_port(occupied);
        assert!(picked > 0);
        assert_ne!(picked, occupied);
    }

    #[test]
    fn health_check_fails_on_closed_port() {
        // 绑定后立即关闭：该端口应不可连接
        let listener = std::net::TcpListener::bind(loopback(0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let sup = Supervisor::new();
        sup.port.store(port as u32, Ordering::SeqCst);
        assert!(!sup.health_check());
    }

    #[test]
    fn health_check_succeeds_on_http_server() {
        // 起一个简易 HTTP 服务，验证 health_check 能识别
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
        // 等待服务接受连接
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if sup.health_check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("health_check should succeed against a live HTTP server");
    }
}
