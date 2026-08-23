use std::path::{Path, PathBuf};
use std::process::Command;

/// Windows 上 Tauri 的 resource_dir() 会返回 `\\?\C:\...` 形式的 verbatim 路径，
/// 而 Node 24 无法用这种路径作为入口脚本（EISDIR lstat 'C:' 崩溃）。
/// 这里去掉 `\\?\` 前缀，转回普通路径。
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

/// Node 二进制在当前平台的相对资源路径（相对 resources 根）
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

/// 解析内置 Node 的绝对路径（resources/<rel>）
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

/// 冒烟：node --version
pub fn smoke(node: &Path) -> Result<String, String> {
    let mut cmd = Command::new(normalize_for_node(node));
    cmd.arg("--version");
    // Windows 下禁止子进程弹控制台窗口（CREATE_NO_WINDOW）
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
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

/// 用内置 Node 运行 install-dsh.mjs 的 prepare 模式（启动路径：基线复制/复用）
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

/// 用内置 Node 运行 install-dsh.mjs 的 check 模式（后台：仅查询最新版）
/// `source` 可选："npmjs" / "npmmirror" / 其他（自动）
pub fn run_check(node: &Path, installer_js: &Path, target: &Path, source: &str) -> Result<String, String> {
    let mut args = vec![
        "check".to_string(),
        "--target".to_string(),
        normalize_for_node(target).to_string_lossy().to_string(),
    ];
    args.extend(registry_args(source));
    run_installer(node, installer_js, &args)
}

/// 用内置 Node 运行 install-dsh.mjs 的 update 模式（后台：安装/更新到最新版）
/// `source` 可选："npmjs" / "npmmirror" / 其他（自动）
pub fn run_update(node: &Path, installer_js: &Path, target: &Path, source: &str) -> Result<String, String> {
    let mut args = vec![
        "update".to_string(),
        "--target".to_string(),
        normalize_for_node(target).to_string_lossy().to_string(),
    ];
    args.extend(registry_args(source));
    run_installer(node, installer_js, &args)
}

/// 根据用户选择的更新源构造 registry 参数（默认自动=不传，由安装器探测）
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
        // auto 或未知：不传，安装器内部 npmjs 优先、失败切 npmmirror
        _ => vec![],
    }
}

/// 用内置 Node 运行 install-dsh.mjs；返回 stdout 全文（调用方解析末行 JSON）
fn run_installer(node: &Path, installer_js: &Path, args: &[String]) -> Result<String, String> {
    let mut cmd = Command::new(normalize_for_node(node));
    cmd.arg(normalize_for_node(installer_js));
    for a in args {
        cmd.arg(a);
    }
    // Windows 下禁止子进程弹控制台窗口（CREATE_NO_WINDOW）
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    log::info!("installer: {:?}", cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("cannot start installer: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !stderr.trim().is_empty() {
        log::warn!("installer stderr: {}", stderr.trim());
    }
    if !out.status.success() {
        // 安装器失败时，stdout 末行仍是 JSON（含白话错误）
        if let Some(line) = stdout.lines().last() {
            return Ok(line.to_string()); // 让调用方解析 ok:false
        }
        return Err(format!(
            "installer exited {}: {}",
            out.status,
            if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() }
        ));
    }
    Ok(stdout)
}

/// 读取 .installed.json 中的版本（若有）
pub fn read_installed_version(runtime_dir: &Path) -> Option<String> {
    let file = runtime_dir.join(".installed.json");
    let text = std::fs::read_to_string(file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(|s| s.to_string())
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
        // Windows: node/win-x64/node.exe；macOS: node/mac-arm64/node；Linux: node/linux-x64/node
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
}
