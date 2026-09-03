
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

pub struct ChildHandle {
    handle: HANDLE,
    pid: u32,
}

unsafe impl Send for ChildHandle {}

impl ChildHandle {
    pub fn id(&self) -> u32 {
        self.pid
    }

    pub fn as_raw_handle(&self) -> HANDLE {
        self.handle
    }

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

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if unsafe { WaitForSingleObject(self.handle, INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code: DWORD = 0;
        unsafe { GetExitCodeProcess(self.handle, &mut code) };
        Ok(ExitStatus::from_raw(code))
    }

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

pub struct SpawnOpts {

    pub program: PathBuf,

    pub args: Vec<String>,

    pub cwd: Option<PathBuf>,

    pub stdout: Option<std::fs::File>,

    pub stderr: Option<std::fs::File>,

    pub suspend: bool,

    pub process_group: bool,
}

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

pub fn spawn_hidden(opts: SpawnOpts) -> io::Result<ChildHandle> {

    let mut cmdline = quote_arg(&opts.program.to_string_lossy());
    for a in &opts.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_arg(a));
    }
    let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().collect();
    cmdline_w.push(0);

    let nul = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")?;
    let use_handles = opts.stdout.is_some() || opts.stderr.is_some();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESHOWWINDOW;
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

    let mut cwd_w: Vec<u16>;
    let cwd_ptr = match &opts.cwd {
        Some(p) => {
            cwd_w = p.as_os_str().encode_wide().collect();
            cwd_w.push(0);
            cwd_w.as_ptr()
        }
        None => std::ptr::null(),
    };

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
            std::ptr::null(),
            cmdline_w.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            if use_handles { TRUE } else { FALSE },
            flags,
            std::ptr::null_mut() as LPVOID,
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
