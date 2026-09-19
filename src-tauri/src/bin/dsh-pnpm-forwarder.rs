//! Native launcher for the bundled pnpm (`pnpm.exe` on Windows).
//!
//! The market calls a bare `pnpm`, and on Windows that used to resolve to a
//! generated `pnpm.cmd`. cmd.exe decodes a batch file with the *console* code
//! page while the file is written with the *OEM* code page, so any install path
//! holding characters outside that page (e.g. `café`) was silently corrupted
//! and pnpm died with "系统找不到指定的路径". A native launcher takes the batch
//! parser, the code page and PATHEXT out of the picture: it resolves the same
//! bundled pair the shim did (Node + `pnpm.mjs`), forwards argv verbatim and
//! exits with the child's status. `.EXE` also wins over any stale `.CMD`.
//!
//! Resource root resolution, in order:
//!  1. `DSH_PNPM_ROOT` (tests, and a manual escape hatch);
//!  2. `<exe dir>` — the installed layout, where tauri copies this binary into
//!     the resource dir next to `node/` and `pnpm/`;
//!  3. `<exe dir>/..` — the dev layout, where cargo builds into
//!     `target/<profile>/` and tauri copies the resources next to it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn node_rel_path() -> PathBuf {
    if cfg!(windows) {
        Path::new("node").join("win-x64").join("node.exe")
    } else if cfg!(target_os = "macos") {
        let plat = if cfg!(target_arch = "aarch64") {
            "mac-arm64"
        } else {
            "mac-x64"
        };
        Path::new("node").join(plat).join("node")
    } else if cfg!(target_arch = "aarch64") {
        Path::new("node").join("linux-arm64").join("node")
    } else {
        Path::new("node").join("linux-x64").join("node")
    }
}

fn pnpm_entry(root: &Path) -> Option<PathBuf> {
    ["pnpm/bin/pnpm.mjs", "pnpm/bin/pnpm.cjs"]
        .iter()
        .map(|rel| root.join(rel))
        .find(|candidate| candidate.is_file())
}

fn resolve_exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate self: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("self has no parent directory: {}", exe.display()))
}

/// The resource root that actually holds `node/` + `pnpm/`, plus its pnpm entry.
pub fn resolve(root_override: Option<&Path>) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = root_override {
        candidates.push(explicit.to_path_buf());
    }
    let exe_dir = resolve_exe_dir()?;
    candidates.push(exe_dir.clone());
    if let Some(parent) = exe_dir.parent() {
        candidates.push(parent.to_path_buf());
    }

    for root in &candidates {
        let node = root.join(node_rel_path());
        let Some(entry) = pnpm_entry(root) else {
            continue;
        };
        if node.is_file() {
            return Ok((root.clone(), node, entry));
        }
    }
    Err(format!(
        "bundled node + pnpm not found near {} (looked in: {})",
        exe_dir.display(),
        candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn main() {
    let override_dir = std::env::var_os("DSH_PNPM_ROOT").map(PathBuf::from);
    let (root, node, entry) = match resolve(override_dir.as_deref()) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("pnpm: {e}");
            eprintln!("pnpm: 程序文件不完整，请重新安装。 / reinstall DSH 工作台.");
            std::process::exit(127);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = Command::new(&node)
        .arg(&entry)
        .args(&args)
        .current_dir(std::env::current_dir().unwrap_or(root))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("pnpm: cannot run {}: {e}", node.display());
            std::process::exit(127);
        }
    }
}
