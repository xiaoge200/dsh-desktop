use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn normalize_for_node(p: &Path) -> PathBuf {
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

pub fn node_rel_path() -> PathBuf {
    let platform = if cfg!(target_os = "windows") {
        "win-x64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { "mac-arm64" } else { "mac-x64" }
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm64"
    } else {
        "linux-x64"
    };
    let exe = if cfg!(target_os = "windows") { "node.exe" } else { "node" };
    Path::new("node").join(platform).join(exe)
}

pub fn resolve_node(resource_dir: &Path) -> std::io::Result<PathBuf> {
    let path = resource_dir.join(node_rel_path());
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("bundled node not found: {}", path.display()),
        ));
    }
    Ok(path)
}

pub fn smoke(node: &Path) -> Result<String, String> {
    let mut cmd = Command::new(normalize_for_node(node));
    cmd.arg("--version");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot execute node {}: {e}", node.display()))?;
    if !out.status.success() {
        return Err(format!(
            "node --version failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        return Err("node --version returned empty".into());
    }
    Ok(v)
}

pub fn run_prepare(
    node: &Path,
    installer_js: &Path,
    target: &Path,
    baseline: &Path,
) -> Result<String, String> {
    run_installer(node, installer_js, &[
        "prepare".to_string(),
        "--target".to_string(),
        normalize_for_node(target).to_string_lossy().to_string(),
        "--baseline".to_string(),
        normalize_for_node(baseline).to_string_lossy().to_string(),
    ])
}

pub fn run_check(node: &Path, installer_js: &Path, target: &Path, source: &str, pre: bool) -> Result<String, String> {
    let mut args = vec![
        "check".to_string(),
        "--target".to_string(),
        normalize_for_node(target).to_string_lossy().to_string(),
    ];
    args.extend(registry_args(source));
    if pre {
        args.push("--pre".to_string());
    }
    run_installer(node, installer_js, &args)
}

pub fn run_update(node: &Path, installer_js: &Path, target: &Path, source: &str, pre: bool) -> Result<String, String> {
    let mut args = vec![
        "update".to_string(),
        "--target".to_string(),
        normalize_for_node(target).to_string_lossy().to_string(),
    ];
    args.extend(registry_args(source));
    if pre {
        args.push("--pre".to_string());
    }
    run_installer(node, installer_js, &args)
}

pub fn run_swap(node: &Path, installer_js: &Path, target: &Path, staging: &Path) -> Result<String, String> {
    let args = vec![
        "swap".to_string(),
        "--target".to_string(),
        normalize_for_node(target).to_string_lossy().to_string(),
        "--staging".to_string(),
        normalize_for_node(staging).to_string_lossy().to_string(),
    ];
    run_installer(node, installer_js, &args)
}

fn registry_args(source: &str) -> Vec<String> {
    match source {
        "npmjs" => vec![
            "--registry".to_string(),
            "https://registry.npmjs.org".to_string(),
            "--mirror".to_string(),
            "https://registry.npmjs.org".to_string(),
        ],
        "npmmirror" => vec![
            "--registry".to_string(),
            "https://registry.npmmirror.com".to_string(),
            "--mirror".to_string(),
            "https://registry.npmmirror.com".to_string(),
        ],

        _ => vec![],
    }
}

fn run_installer(node: &Path, installer_js: &Path, args: &[String]) -> Result<String, String> {
    let mut full_args: Vec<String> = vec![normalize_for_node(installer_js).to_string_lossy().to_string()];
    full_args.extend(args.iter().cloned());
    log::info!("installer: {:?} {:?}", normalize_for_node(node), full_args);

    #[cfg(windows)]
    let (success, stdout, stderr) = {
        use crate::winproc::{create_pipe, spawn_hidden, SpawnOpts};
        let (out_r, out_w) = create_pipe().map_err(|e| format!("cannot start installer: {e}"))?;
        let (err_r, err_w) = create_pipe().map_err(|e| format!("cannot start installer: {e}"))?;
        let mut child = spawn_hidden(SpawnOpts {
            program: normalize_for_node(node),
            args: full_args,
            cwd: None,
            stdout: Some(out_w),
            stderr: Some(err_w),
            suspend: false,
            process_group: false,
        })
        .map_err(|e| format!("cannot start installer: {e}"))?;

        let t_out = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let _ = (&mut &out_r).read_to_string(&mut buf);
            buf
        });
        let t_err = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            let _ = (&mut &err_r).read_to_string(&mut buf);
            buf
        });
        let status = child.wait().map_err(|e| format!("installer wait failed: {e}"))?;
        (
            status.success(),
            t_out.join().unwrap_or_default(),
            t_err.join().unwrap_or_default(),
        )
    };
    #[cfg(unix)]
    let (success, stdout, stderr) = {
        let mut cmd = Command::new(normalize_for_node(node));
        cmd.args(&full_args);
        log::info!("installer: {:?}", cmd);
        let out = cmd
            .output()
            .map_err(|e| format!("cannot start installer: {e}"))?;
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    if !stderr.trim().is_empty() {
        log::warn!("installer stderr: {}", stderr.trim());
    }
    if !success {

        if let Some(line) = stdout.lines().last() {
            return Ok(line.to_string());
        }
        return Err(format!(
            "installer exited: {}",
            if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() }
        ));
    }
    Ok(stdout)
}

pub fn read_installed_version(runtime_dir: &Path) -> Option<String> {
    let file = runtime_dir.join(".installed.json");
    let text = std::fs::read_to_string(file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(|s| s.to_string())
}

#[derive(Debug, Clone, Default)]
pub struct InstallerResult {
    pub ok: bool,

    pub action: String,

    pub version: Option<String>,

    pub prerelease: Option<String>,

    pub pre_available: bool,

    pub current: Option<String>,

    pub staging: Option<String>,

    pub message: Option<String>,

    pub detail: Option<String>,
}

pub fn parse_installer_output(out: &str) -> InstallerResult {
    let mut res = InstallerResult::default();
    let Some(line) = out.lines().last() else { return res };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(line) else { return res };
    res.ok = json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    res.action = json.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string();
    res.version = json.get("version").and_then(|v| v.as_str()).map(|s| s.to_string());
    res.prerelease = json.get("prerelease").and_then(|v| v.as_str()).map(|s| s.to_string());
    res.pre_available = json.get("pre_available").and_then(|v| v.as_bool()).unwrap_or(false);
    res.current = json.get("current").and_then(|v| v.as_str()).map(|s| s.to_string());
    res.staging = json.get("staging").and_then(|v| v.as_str()).map(|s| s.to_string());
    res.message = json
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    res.detail = json
        .pointer("/error/detail")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_rel_path_has_platform_dir_and_exe() {
        let p = node_rel_path();
        let s = p.to_string_lossy().to_string();
        let prefix = format!("node{}", std::path::MAIN_SEPARATOR);
        assert!(s.starts_with(&prefix));

        if cfg!(windows) {
            assert!(s.ends_with("node.exe"));
            assert!(s.contains("win-x64"));
        } else if cfg!(target_os = "macos") {
            assert!(s.ends_with("node"));
            assert!(s.contains("mac-"));
        } else {
            assert!(s.ends_with("node"));
            assert!(s.contains("linux-"));
        }
    }
 
    #[test]
    fn read_installed_version_parses_json() {
        let dir = std::env::temp_dir().join(format!("dsh-node-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".installed.json"),
            r#"{"version":"1.2.3-rc.1","source":"baseline"}"#,
        )
        .unwrap();
        assert_eq!(read_installed_version(&dir).as_deref(), Some("1.2.3-rc.1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_installed_version_missing_file_returns_none() {
        let dir = std::env::temp_dir().join(format!("dsh-node-test-none-{}", std::process::id()));
        assert_eq!(read_installed_version(&dir), None);
    }

    #[test]
    fn read_installed_version_bad_json_returns_none() {
        let dir = std::env::temp_dir().join(format!("dsh-node-test-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".installed.json"), "not json").unwrap();
        assert_eq!(read_installed_version(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_installer_output_new_version() {
        let r = parse_installer_output(
            "log line\n{\"ok\":true,\"action\":\"new-version-available\",\"version\":\"1.5.0\",\"current\":\"1.4.0\",\"dir\":\"x\",\"source\":\"npmjs\"}",
        );
        assert!(r.ok);
        assert_eq!(r.action, "new-version-available");
        assert_eq!(r.version.as_deref(), Some("1.5.0"));
        assert_eq!(r.current.as_deref(), Some("1.4.0"));
    }

    #[test]
    fn parse_installer_output_error() {
        let r = parse_installer_output(
            "{\"ok\":false,\"error\":{\"kind\":\"network\",\"message\":\"暂时无法检查更新\",\"detail\":\"both registries unreachable\"}}",
        );
        assert!(!r.ok);
        assert_eq!(r.message.as_deref(), Some("暂时无法检查更新"));
    }

    #[test]
    fn parse_installer_output_garbage_returns_defaults() {
        let r = parse_installer_output("not json at all");
        assert!(!r.ok);
        assert!(r.action.is_empty());
        assert!(r.version.is_none());
    }
}
