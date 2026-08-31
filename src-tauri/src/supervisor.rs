use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 平台相关的子进程句柄：Windows 用隐藏控制台的 ChildHandle（其子孙进程也不弹窗），
/// 其他平台用 std::process::Child。
#[cfg(windows)]
type SpawnedChild = crate::winproc::ChildHandle;
#[cfg(not(windows))]
type SpawnedChild = std::process::Child;

/// Windows Job Object 封装：把服务进程树挂到主进程名下，主进程无论以何种方式
/// 退出（含被强杀/崩溃），job 内进程随之终止，杜绝孤儿 node 进程。
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

    /// Job 句柄。裸 HANDLE 不是 Send/Sync，这里包一层标记。
    pub struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// 创建带 KILL_ON_JOB_CLOSE 的 Job；失败返回 None（调用方降级为 taskkill 清理）。
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

    /// 把子进程加入 job（Win8+ 允许嵌套 job，一般会成功；失败不致命）。
    pub fn assign(job: &Job, child_handle: HANDLE) -> bool {
        unsafe { AssignProcessToJobObject(job.0, child_handle) != 0 }
    }

    /// 恢复被 CREATE_SUSPENDED 挂起的进程全部线程（Toolhelp 枚举）。
    /// 刚创建即挂起的进程只有主线程，恢复后即正常运行。
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

    /// 终止 job 内全部进程（正常 stop/退出路径；幂等）。
    pub fn terminate(job: &Job) {
        unsafe { TerminateJobObject(job.0, 1) };
    }
}

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
/// - 退出清理：Windows 挂 Job Object（KILL_ON_JOB_CLOSE）——主进程无论以何种方式
///   退出（含强杀/崩溃）服务进程树都随之终止；再叠加 taskkill /T /F 兜底
pub struct Supervisor {
    child: Mutex<Option<SpawnedChild>>,
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
    /// Windows Job 对象（KILL_ON_JOB_CLOSE）：主进程被强杀时服务进程树随之终止
    #[cfg(windows)]
    job: Mutex<Option<win::Job>>,
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
            #[cfg(windows)]
            job: Mutex::new(None),
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

        // 组装命令行参数（两平台共用）：node <dsh_bin> web --no-open --port N [extra...]
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

        // Windows：用「隐藏控制台」启动——node 自身及其全部后代（dsh worker、
        // npm 脚本等）都继承同一个隐藏控制台，整棵进程树都不弹命令行窗口。
        // （CREATE_NO_WINDOW 只隐藏直接进程，node 的后代没有可继承控制台时会
        //   各自新建可见控制台 → 插件更新/重启时闪黑窗。详见 winproc 模块注释。）
        // CREATE_SUSPENDED：先挂起，等挂进 Job 后再恢复，避免启动竞态。
        #[cfg(windows)]
        let child: SpawnedChild = {
            use crate::winproc::{spawn_hidden, SpawnOpts};
            let child = spawn_hidden(SpawnOpts {
                program: normalize_for_node(node),
                args,
                cwd: Some(normalize_for_node(workspace)),
                stdout: stdout_file,
                stderr: stderr_file,
                suspend: true,
                process_group: true,
            })
            .map_err(|e| format!("启动服务失败: {e}"))?;
            log::info!("service spawned (hidden console), pid={}", child.id());

            // 把服务挂进 KILL_ON_JOB_CLOSE 的 Job——主进程无论以何种方式退出
            // （含强杀/崩溃），整棵服务进程树都会随之终止，杜绝孤儿进程。
            let job = win::create_kill_on_close_job();
            if let Some(ref j) = job {
                if win::assign(j, child.as_raw_handle()) {
                    log::info!("service attached to kill-on-close job");
                } else {
                    log::warn!("assign service to job failed; falling back to taskkill cleanup");
                }
            }
            // 无论 job 是否创建成功，都要恢复被挂起的进程
            win::resume_threads(child.id());
            *self.job.lock().unwrap() = job;
            child
        };

        // Unix：进程组便于退出时整组清理；stdout/stderr 落盘或丢弃
        #[cfg(unix)]
        let child: SpawnedChild = {
            let mut cmd = Command::new(normalize_for_node(node));
            cmd.args(&args).current_dir(normalize_for_node(workspace));
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
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
            log::info!("spawn: {:?}", cmd);
            cmd.spawn().map_err(|e| format!("启动服务失败: {e}"))?
        };

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
        // Windows：先终止 Job 内全部进程（含游离子进程），再走常规清理兜底
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
