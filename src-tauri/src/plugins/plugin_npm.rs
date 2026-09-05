use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const NPM_TIMEOUT: Duration = Duration::from_secs(600);
pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(1800);

fn npm_cli_path(node: &Path) -> Option<PathBuf> {
    let node_dir = node.parent()?;
    let candidates = [
        node_dir.join("node_modules").join("npm").join("bin").join("npm-cli.js"),
        node_dir.join("..").join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"),
        node_dir.join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

pub(crate) fn registry_flag(source: &str) -> Vec<String> {
    match source {
        "npmjs" => vec!["--registry".into(), "https://registry.npmjs.org".into()],
        "npmmirror" => vec!["--registry".into(), "https://registry.npmmirror.com".into()],
        _ => vec![],
    }
}

fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    if count <= n {
        return text.to_string();
    }
    text.chars().skip(count - n).collect()
}

fn friendly_npm_error(stderr: &str, stdout: &str) -> String {
    let combined = format!("{stderr}\n{stdout}");
    let errs: Vec<&str> = combined
        .lines()
        .filter(|l| l.starts_with("npm error") || l.starts_with("npm ERR!"))
        .map(|l| l.trim())
        .collect();
    let detail = if !errs.is_empty() {
        errs.into_iter().take(6).collect::<Vec<_>>().join("\n")
    } else {
        tail(combined.trim(), 600)
    };
    format!("安装失败：\n{detail}")
}

pub(crate) fn run_npm(node: &Path, cwd: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
    let npm_cli = npm_cli_path(node).ok_or("内置 npm 不完整，请重新安装本应用。")?;
    let node_n = crate::node::normalize_for_node(node);
    let npm_n = crate::node::normalize_for_node(&npm_cli);
    let cwd_n = crate::node::normalize_for_node(cwd);
    let mut full_args: Vec<String> = vec![npm_n.to_string_lossy().to_string()];
    full_args.extend(args.iter().cloned());
    log::info!("npm: {:?} {:?}", node_n, full_args);

    #[cfg(windows)]
    let (mut child, pipes): (
        crate::winproc::ChildHandle,
        Vec<(bool, Option<Box<dyn std::io::Read + Send>>)>,
    ) = {
        use crate::winproc::{create_pipe, spawn_hidden, SpawnOpts};
        let (out_r, out_w) = create_pipe().map_err(|e| format!("无法创建管道: {e}"))?;
        let (err_r, err_w) = create_pipe().map_err(|e| format!("无法创建管道: {e}"))?;
        let child = spawn_hidden(SpawnOpts {
            program: node_n.clone(),
            args: full_args,
            cwd: Some(cwd_n.clone()),
            stdout: Some(out_w),
            stderr: Some(err_w),
            suspend: false,
            process_group: true,
        })
        .map_err(|e| format!("无法启动 npm: {e}"))?;
        let pipes: Vec<(bool, Option<Box<dyn std::io::Read + Send>>)> = vec![
            (false, Some(Box::new(out_r) as Box<dyn std::io::Read + Send>)),
            (true, Some(Box::new(err_r) as Box<dyn std::io::Read + Send>)),
        ];
        (child, pipes)
    };
    #[cfg(unix)]
    let (mut child, pipes): (
        std::process::Child,
        Vec<(bool, Option<Box<dyn std::io::Read + Send>>)>,
    ) = {
        let mut cmd = Command::new(&node_n);
        cmd.args(&full_args).current_dir(&cwd_n);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| format!("无法启动 npm: {e}"))?;
        let pipes: Vec<(bool, Option<Box<dyn std::io::Read + Send>>)> = vec![
            (false, child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
            (true, child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
        ];
        (child, pipes)
    };

    let (tx, rx) = std::sync::mpsc::channel::<(bool, String)>();
    for (is_err, pipe) in pipes {
        let tx = tx.clone();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = String::new();
                let _ = pipe.read_to_string(&mut buf);
                let _ = tx.send((is_err, buf));
            });
        }
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                loop {
                    match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok((true, t)) => err.push_str(&t),
                        Ok((false, t)) => out.push_str(&t),
                        Err(_) => break,
                    }
                }
                if status.success() {
                    log::info!("npm ok: {}", out.lines().next().unwrap_or(""));
                    return Ok(());
                }
                log::error!("npm failed ({}): {}", status, tail(&err, 2000));
                return Err(friendly_npm_error(&err, &out));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("npm 进程异常: {e}")),
        }
        if Instant::now() >= deadline {
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
            }
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err("安装超时（已终止），请检查网络后重试。".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
