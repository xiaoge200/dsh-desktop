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
        }
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

    /// 启动服务：node <dsh>/lib/bin.js web --no-open --port <port>
    /// 返回实际使用的端口（0 表示启动失败）。
    pub fn start(
        &mut self,
        node: &Path,
        dsh_bin: &Path,
        workspace: &Path,
        preferred_port: u16,
    ) -> Result<u16, String> {
        self.stop();

        let port = Self::pick_free_port(preferred_port);
        if port == 0 {
            return Err("无法确定可用端口".into());
        }

        let mut cmd = Command::new(normalize_for_node(node));
        cmd.arg(normalize_for_node(dsh_bin))
            .arg("web")
            .arg("--no-open")
            .arg("--port")
            .arg(port.to_string())
            .current_dir(normalize_for_node(workspace))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // 进程组：便于退出时清理整棵进程树
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
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
    pub fn restart(&mut self, node: &Path, dsh_bin: &Path, workspace: &Path, preferred_port: u16) -> Result<u16, String> {
        if !self.allow_restart() {
            log::warn!("service exceeded restart limit; degraded mode");
            return Err("服务多次启动失败".into());
        }
        log::warn!("restarting service");
        self.start(node, dsh_bin, workspace, preferred_port)
    }

    /// 停止服务并清理进程树。
    pub fn stop(&mut self) {
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            log::info!("stopping service pid={}", child.id());
            let pid = child.id();
            #[cfg(windows)]
            {
                // taskkill /T /F 杀整棵树
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
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
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}
