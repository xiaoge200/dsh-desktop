//! Windows 专用：以「隐藏控制台」方式启动子进程，让**整棵进程树**都不闪现命令行窗口。
//!
//! 为什么不用 CREATE_NO_WINDOW：该标志只作用于**直接创建的那个进程**。node 被隐藏后，
//! 它内部再派生的子进程（dsh 的 worker、npm 的 cmd 脚本等）没有可继承的控制台，Windows
//! 会为它们**新建一个可见控制台**——于是插件更新、重启服务时会一闪而过黑窗口。
//! 这里改用 CREATE_NEW_CONSOLE + STARTUPINFO.wShowWindow=SW_HIDE：给 node 一个
//! **隐藏的控制台**，其全部后代都继承这个隐藏控制台，整棵树不再弹窗。
//!
//! 需要自建 CreateProcessW（std::process::Command 无法设置 STARTUPINFO.wShowWindow）。

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use winapi::shared::minwindef::{DWORD, FALSE, LPVOID, TRUE};
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::handleapi::{CloseHandle, SetHandleInformation, INVALID_HANDLE_VALUE};
use winapi::um::namedpipeapi::CreatePipe;
use winapi::um::processthreadsapi::{
    CreateProcessW, GetExitCodeProcess, TerminateProcess, PROCESS_INFORMATION, STARTUPINFOW,
};
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{
    CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    HANDLE_FLAG_INHERIT, INFINITE, STARTF_USESTDHANDLES, STARTF_USESHOWWINDOW, WAIT_OBJECT_0,
};
use winapi::um::winnt::HANDLE;
use winapi::um::winuser::SW_HIDE;

/// 隐藏控制台启动的子进程句柄封装（替代 std::process::Child 在 Windows 上的用途）。
pub struct ChildHandle {
    handle: HANDLE,
    pid: u32,
}

// HANDLE 本身不是 Send，但进程句柄可安全跨线程使用（仅 CloseHandle 时需要独占）。
unsafe impl Send for ChildHandle {}

impl ChildHandle {
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// 原始进程句柄（供挂 Job Object 等使用）。
    pub fn as_raw_handle(&self) -> HANDLE {
        self.handle
    }

    /// 非阻塞查询是否已退出；已退出返回退出码。
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let rc = unsafe { WaitForSingleObject(self.handle, 0) };
        if rc == WAIT_OBJECT_0 {
            let mut code: DWORD = 0;
            if unsafe { GetExitCodeProcess(self.handle, &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(ExitStatus::from_raw(code)))
        } else if rc == WAIT_TIMEOUT {
            Ok(None)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// 阻塞等待退出，返回退出码。
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if unsafe { WaitForSingleObject(self.handle, INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code: DWORD = 0;
        unsafe { GetExitCodeProcess(self.handle, &mut code) };
        Ok(ExitStatus::from_raw(code))
    }

    /// 强制终止进程。
    pub fn kill(&mut self) -> io::Result<()> {
        let ok = unsafe { TerminateProcess(self.handle, 1) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for ChildHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// 启动参数。
pub struct SpawnOpts {
    /// 可执行文件绝对路径
    pub program: PathBuf,
    /// 命令行参数（不含程序本身）
    pub args: Vec<String>,
    /// 工作目录；None 继承父进程
    pub cwd: Option<PathBuf>,
    /// stdout 重定向句柄；None → NUL（丢弃）
    pub stdout: Option<std::fs::File>,
    /// stderr 重定向句柄；None → NUL（丢弃）
    pub stderr: Option<std::fs::File>,
    /// 是否 CREATE_SUSPENDED（挂起后由调用方挂 Job / 恢复）
    pub suspend: bool,
    /// 是否 CREATE_NEW_PROCESS_GROUP（便于整组清理）
    pub process_group: bool,
}

/// 创建一对匿名管道，返回 (读端, 写端)；写端已标记可继承（交给子进程），读端留在父进程。
pub fn create_pipe() -> io::Result<(std::fs::File, std::fs::File)> {
    unsafe {
        let mut read: HANDLE = INVALID_HANDLE_VALUE;
        let mut write: HANDLE = INVALID_HANDLE_VALUE;
        if CreatePipe(&mut read, &mut write, std::ptr::null_mut(), 0) == 0 {
            return Err(io::Error::last_os_error());
        }
        SetHandleInformation(write, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        Ok((
            std::fs::File::from_raw_handle(read as RawHandle),
            std::fs::File::from_raw_handle(write as RawHandle),
        ))
    }
}

/// 以隐藏控制台方式启动进程；成功返回句柄封装。
pub fn spawn_hidden(opts: SpawnOpts) -> io::Result<ChildHandle> {
    // ---- 命令行（按 CommandLineToArgvW 规则加引号）----
    let mut cmdline = quote_arg(&opts.program.to_string_lossy());
    for a in &opts.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_arg(a));
    }
    let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().collect();
    cmdline_w.push(0);

    // ---- stdio：显式句柄时用 STARTF_USESTDHANDLES，缺省用 NUL ----
    let nul = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")?;
    let use_handles = opts.stdout.is_some() || opts.stderr.is_some();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESHOWWINDOW; // 配合 wShowWindow=SW_HIDE 隐藏新控制台窗口
    si.wShowWindow = SW_HIDE as u16;
    if use_handles {
        si.dwFlags |= STARTF_USESTDHANDLES;
        si.hStdInput = nul.as_raw_handle() as HANDLE;
        si.hStdOutput = match &opts.stdout {
            Some(f) => {
                let h = f.as_raw_handle() as HANDLE;
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                h
            }
            None => nul.as_raw_handle() as HANDLE,
        };
        si.hStdError = match &opts.stderr {
            Some(f) => {
                let h = f.as_raw_handle() as HANDLE;
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                h
            }
            None => nul.as_raw_handle() as HANDLE,
        };
        unsafe {
            SetHandleInformation(
                nul.as_raw_handle() as HANDLE,
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        };
    }

    // ---- 工作目录 ----
    let mut cwd_w: Vec<u16>;
    let cwd_ptr = match &opts.cwd {
        Some(p) => {
            cwd_w = p.as_os_str().encode_wide().collect();
            cwd_w.push(0);
            cwd_w.as_ptr()
        }
        None => std::ptr::null(),
    };

    // ---- 创建标志：隐藏控制台 + 可选挂起/进程组 ----
    let mut flags: DWORD = CREATE_NEW_CONSOLE | CREATE_UNICODE_ENVIRONMENT;
    if opts.suspend {
        flags |= CREATE_SUSPENDED;
    }
    if opts.process_group {
        flags |= CREATE_NEW_PROCESS_GROUP;
    }

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),           // lpApplicationName：用命令行首 token 定位
            cmdline_w.as_mut_ptr(),     // lpCommandLine（CreateProcessW 可能改写，需可变）
            std::ptr::null_mut(),       // lpProcessAttributes
            std::ptr::null_mut(),       // lpThreadAttributes
            if use_handles { TRUE } else { FALSE }, // bInheritHandles
            flags,
            std::ptr::null_mut() as LPVOID, // lpEnvironment：继承父进程环境
            cwd_ptr,
            &mut si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { CloseHandle(pi.hThread) };
    Ok(ChildHandle {
        handle: pi.hProcess,
        pid: pi.dwProcessId,
    })
}

/// 按 CommandLineToArgvW 规则给单个参数加引号（含空格/引号/空串时）。
fn quote_arg(s: &str) -> String {
    if !s.is_empty() && !s.contains([' ', '\t', '"']) {
        return s.to_string();
    }
    let mut out = String::from("\"");
    let mut bs = 0usize;
    for ch in s.chars() {
        match ch {
            '\\' => bs += 1,
            '"' => {
                out.push_str(&"\\".repeat(bs * 2 + 1));
                out.push('"');
                bs = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(bs));
                out.push(ch);
                bs = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(bs * 2));
    out.push('"');
    out
}
