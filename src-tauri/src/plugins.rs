
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

const PROFILE_NAME: &str = "web";
const PROFILE_PATCH_FILENAME: &str = "cordis.patch.yml";
const SNAPSHOT_FILE: &str = "plugins-state.json";
const DEFAULT_PLUGIN: &str = "dshmarket";
const DEFAULT_PLUGIN_MARKER: &str = "default-plugin.json";

const TEMPLATE_BUNDLES: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];

const PROFILE_PATCH_TEMPLATE: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n\
# a top-level YAML array of loader patch entries (id-targeted config\n\
# overrides, disables, and insert lists; `!!js` expressions allowed).\n\
[]\n";

const PROFILE_PNPM_WORKSPACE: &str = "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n";

const NPM_TIMEOUT: Duration = Duration::from_secs(600);
const GIT_TIMEOUT: Duration = Duration::from_secs(1800);

static PLUGIN_OPS: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
pub struct PluginRow {
    pub id: String,
    pub name: Option<String>,
    pub enabled: bool,
    pub managed: bool,
    pub home_layer: bool,
}

#[derive(Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub spec: String,
    pub source: String,
    pub builtin: bool,
    pub is_bundle: bool,
    pub rows: Vec<PluginRow>,
    pub restart_required: bool,
}

#[derive(Serialize)]
pub struct PluginsListData {
    pub plugins: Vec<PluginInfo>,
    pub service_restart_required: bool,
    pub profile_dir: String,
    pub initialized: bool,
}

#[derive(Serialize)]
pub struct PluginAddResult {
    pub name: String,
    pub version: String,
    pub restart_required: bool,
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct MarketplaceItem {
    pub name: String,
    pub description: String,
    pub source: String,
    pub spec: String,
    pub url: String,
}

#[derive(Serialize)]
pub struct MarketplaceResult {
    pub items: Vec<MarketplaceItem>,
    pub errors: Vec<String>,
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(h) = dirs::home_dir() {
            return h;
        }
        return PathBuf::from(path);
    }
    if path.starts_with("~/") || path.starts_with("~\\") {
        if let Some(h) = dirs::home_dir() {
            return h.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

fn make_absolute(p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .map(|c| c.join(&p))
            .unwrap_or(p)
    }
}

pub fn dsh_home() -> PathBuf {
    let configured = std::env::var("DSH_HOME").ok().filter(|v| !v.trim().is_empty());
    match configured {
        Some(p) => make_absolute(expand_tilde(&p)),
        None => dirs::home_dir()
            .map(|h| h.join(".dsh"))
            .unwrap_or_else(|| PathBuf::from(".dsh")),
    }
}

pub fn profile_dir(home: &Path) -> PathBuf {
    home.join("profiles").join(PROFILE_NAME)
}

pub fn ensure_profile(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 profile 目录失败: {e}"))?;
    let manifest_path = dir.join("package.json");
    if !manifest_path.exists() {
        let basename = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(PROFILE_NAME);
        let manifest = serde_json::json!({
            "name": format!("dsh-profile-{basename}"),
            "private": true,
            "dependencies": {},
            "dsh": { "profile": { "bundles": TEMPLATE_BUNDLES } }
        });
        write_manifest(dir, &manifest)?;
        log::info!("plugins: initialized profile at {}", dir.display());
    }
    let patch_path = dir.join(PROFILE_PATCH_FILENAME);
    if !patch_path.exists() {
        std::fs::write(&patch_path, PROFILE_PATCH_TEMPLATE)
            .map_err(|e| format!("创建补丁文件失败: {e}"))?;
    }
    let workspace_path = dir.join("pnpm-workspace.yaml");
    if !workspace_path.exists() {
        std::fs::write(&workspace_path, PROFILE_PNPM_WORKSPACE)
            .map_err(|e| format!("创建 pnpm-workspace 失败: {e}"))?;
    }
    Ok(())
}

fn read_manifest(dir: &Path) -> Result<serde_json::Value, String> {
    let path = dir.join("package.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 profile 配置失败（{}）: {e}", path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("profile 配置不是有效 JSON（{}）: {e}", path.display()))?;
    if !parsed.is_object() {
        return Err(format!("profile 配置必须是 JSON 对象（{}）", path.display()));
    }
    Ok(parsed)
}

fn write_manifest(dir: &Path, manifest: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())? + "\n";
    std::fs::write(dir.join("package.json"), text).map_err(|e| format!("写入 profile 配置失败: {e}"))
}

fn dep_keys(manifest: &serde_json::Value) -> Vec<String> {
    manifest
        .get("dependencies")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

fn read_version(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("version").and_then(|s| s.as_str()).map(|s| s.to_string())
}

fn package_dir_from_anchor(anchor_file: &Path, package_name: &str) -> Option<PathBuf> {
    let mut d = anchor_file.parent()?;
    loop {
        let candidate = d.join("node_modules").join(package_name);
        if candidate.join("package.json").exists() {
            return Some(candidate);
        }
        d = d.parent()?;
    }
}

fn resolve_bundle_dir(runtime: &Path, profile: &Path, package_name: &str) -> Option<PathBuf> {
    let install_anchor = runtime
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("package.json");
    for anchor in [install_anchor, profile.join("package.json")] {
        if let Some(dir) = package_dir_from_anchor(&anchor, package_name) {
            return Some(dir);
        }
    }
    None
}

fn exports_patch(runtime: &Path, profile: &Path, package_name: &str) -> bool {
    let Some(dir) = resolve_bundle_dir(runtime, profile, package_name) else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    manifest
        .pointer("/dsh/bundle/patch")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

fn reconcile(
    before_deps: &HashSet<String>,
    runtime: &Path,
    profile: &Path,
) -> Result<bool, String> {
    let mut after = read_manifest(profile)?;
    let dependencies = dep_keys(&after);
    let mut bundles: Vec<String> = after
        .pointer("/dsh/profile/bundles")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let mut changed = false;

    for name in &dependencies {
        let is_bundle = exports_patch(runtime, profile, name);
        if is_bundle && !bundles.contains(name) {
            bundles.push(name.clone());
            changed = true;
        } else if !is_bundle && !before_deps.contains(name) {
            log::warn!(
                "dsh: warning: {name} declares no dsh.bundle — installed as a plain dependency, not a profile layer (a later update that gains one activates it automatically)"
            );
        }
    }
    let dependency_set: HashSet<&String> = dependencies.iter().collect();
    let mut remaining = Vec::new();
    for name in &bundles {
        let was_dependency = before_deps.contains(name) || dependency_set.contains(name);
        let still_bundle = dependency_set.contains(name) && exports_patch(runtime, profile, name);
        if was_dependency && !still_bundle {
            changed = true;
        } else {
            remaining.push(name.clone());
        }
    }
    if !changed {
        return Ok(false);
    }
    bundles = remaining;
    let bundles_value =
        serde_json::Value::Array(bundles.into_iter().map(serde_json::Value::String).collect());
    if let Some(dsh) = after.get_mut("dsh").and_then(|v| v.as_object_mut()) {
        if let Some(p) = dsh.get_mut("profile").and_then(|v| v.as_object_mut()) {
            p.insert("bundles".into(), bundles_value);
        } else {
            dsh.insert("profile".into(), serde_json::json!({ "bundles": bundles_value }));
        }
    } else {
        after
            .as_object_mut()
            .unwrap()
            .insert("dsh".into(), serde_json::json!({ "profile": { "bundles": bundles_value } }));
    }
    write_manifest(profile, &after)?;
    Ok(true)
}

struct RowPatch {
    id: String,
    name: Option<String>,
    disabled: Option<bool>,
}

fn yaml_get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

fn row_patches(entries: &[serde_yaml::Value]) -> Vec<RowPatch> {
    let mut out = Vec::new();
    let push = |v: &serde_yaml::Value, out: &mut Vec<RowPatch>| {
        let Some(map) = v.as_mapping() else { return };
        let id = yaml_get(map, "id").and_then(|v| v.as_str()).map(|s| s.to_string());
        let Some(id) = id else { return };
        let name = yaml_get(map, "name").and_then(|v| v.as_str()).map(|s| s.to_string());
        let disabled = yaml_get(map, "disabled").and_then(|v| v.as_bool());
        out.push(RowPatch { id, name, disabled });
    };
    for entry in entries {
        if let Some(insert) = entry
            .as_mapping()
            .and_then(|m| yaml_get(m, "insert"))
            .and_then(|v| v.as_sequence())
        {
            for item in insert {
                push(item, &mut out);
            }
        }
        push(entry, &mut out);
    }
    out
}

fn parse_patch_file_opt(path: &Path) -> Option<Vec<serde_yaml::Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    match serde_yaml::from_str::<serde_yaml::Value>(text) {
        Ok(serde_yaml::Value::Sequence(seq)) => Some(seq),
        Ok(_) => {
            log::warn!("patch file {} is not a top-level sequence", path.display());
            None
        }
        Err(e) => {
            log::warn!("failed to parse patch file {}: {e}", path.display());
            None
        }
    }
}

fn bundle_row_patches(dir: &Path) -> Vec<RowPatch> {
    let Ok(raw) = std::fs::read_to_string(dir.join("package.json")) else {
        return Vec::new();
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(patch_rel) = manifest.pointer("/dsh/bundle/patch").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    let entries = match parse_patch_file_opt(&dir.join(patch_rel)) {
        Some(e) => e,
        None => return Vec::new(),
    };
    row_patches(&entries)
}

fn append_disable_row(patch_path: &Path, id: &str, enabled: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(patch_path)
        .map_err(|e| format!("读取补丁文件失败（{}）: {e}", patch_path.display()))?;
    let parsed: serde_yaml::Value = serde_yaml::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| "cordis.patch.yml 无法解析（可能被手动改坏），请修复后重试。".to_string())?;
    if !matches!(parsed, serde_yaml::Value::Sequence(_)) {
        return Err("cordis.patch.yml 必须是顶层 YAML 数组（补丁条目列表）。".into());
    }

    let mut m = serde_yaml::Mapping::new();
    m.insert(serde_yaml::Value::String("id".into()), serde_yaml::Value::String(id.to_string()));
    m.insert(
        serde_yaml::Value::String("disabled".into()),
        serde_yaml::Value::Bool(enabled),
    );
    let row = serde_yaml::to_string(&serde_yaml::Value::Mapping(m.clone()))
        .map_err(|e| format!("生成补丁行失败: {e}"))?;
    let lines: Vec<&str> = row.lines().filter(|l| !l.trim().is_empty()).collect();
    let block = lines
        .iter()
        .enumerate()
        .map(|(i, l)| if i == 0 { format!("- {l}") } else { format!("  {l}") })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let file_lines: Vec<&str> = text.lines().collect();
    let last_idx = file_lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .ok_or("cordis.patch.yml 内容为空，请先修复。")?;
    let last_line = file_lines[last_idx].trim();

    let new_text = if last_line == "[]" {
        let mut out: Vec<String> = file_lines.iter().map(|l| l.to_string()).collect();
        out[last_idx] = block;
        out.join("\n") + "\n"
    } else if file_lines.iter().any(|l| l.trim_start().starts_with("- ")) {
        format!("{}\n{}", text.trim_end(), block)
    } else {
        log::warn!("cordis.patch.yml 使用流式写法，切换插件将重写文件（注释可能丢失）");
        let mut seq = match parsed {
            serde_yaml::Value::Sequence(s) => s,
            _ => unreachable!(),
        };
        seq.push(serde_yaml::Value::Mapping(m));
        serde_yaml::to_string(&serde_yaml::Value::Sequence(seq))
            .map_err(|e| format!("重写补丁文件失败: {e}"))?
    };

    let tmp = patch_path.with_extension("yml.tmp");
    std::fs::write(&tmp, &new_text).map_err(|e| format!("写入补丁文件失败: {e}"))?;
    std::fs::rename(&tmp, patch_path).map_err(|e| format!("替换补丁文件失败: {e}"))?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    profile: String,
    recorded_at: u64,
    bundles: Vec<SnapshotBundle>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotBundle {
    name: String,
    version: Option<String>,
}

fn snapshot_path(app_data: &Path) -> PathBuf {
    app_data.join(SNAPSHOT_FILE)
}

fn read_snapshot(app_data: &Path) -> Option<Snapshot> {
    let raw = std::fs::read_to_string(snapshot_path(app_data)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn snapshot_differs(snap: &Option<Snapshot>, name: &str, version: &str) -> bool {
    let Some(s) = snap else { return false };
    match s.bundles.iter().find(|b| b.name == name) {
        None => true,
        Some(b) => version != "-" && b.version.as_deref() != Some(version),
    }
}

pub fn record_restart(state: &AppState) {
    let Ok(_guard) = PLUGIN_OPS.try_lock() else {
        log::warn!("plugins: snapshot skipped (plugin op in progress)");
        return;
    };
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return;
    }
    let app_data = state.app_data_dir();
    if app_data.as_os_str().is_empty() {
        return;
    }
    let profile = profile_dir(&dsh_home());
    let Ok(manifest) = read_manifest(&profile) else {
        return;
    };
    let Some(bundles) = manifest.pointer("/dsh/profile/bundles").and_then(|v| v.as_array()) else {
        return;
    };
    let mut out = Vec::new();
    for b in bundles {
        let Some(name) = b.as_str() else { continue };
        let version = resolve_bundle_dir(&runtime, &profile, name).and_then(|d| read_version(&d));
        out.push(SnapshotBundle { name: name.to_string(), version });
    }
    let snapshot = Snapshot {
        profile: PROFILE_NAME.to_string(),
        recorded_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        bundles: out,
    };
    if let Err(e) = std::fs::write(
        snapshot_path(&app_data),
        serde_json::to_string_pretty(&snapshot).unwrap_or_default(),
    ) {
        log::warn!("plugins: write snapshot failed: {e}");
    }
}

fn npm_cli_path(node: &Path) -> Option<PathBuf> {
    let node_dir = node.parent()?;
    let candidates = [
        node_dir.join("node_modules").join("npm").join("bin").join("npm-cli.js"),
        node_dir.join("..").join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"),
        node_dir.join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn registry_flag(source: &str) -> Vec<String> {
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

fn run_npm(node: &Path, cwd: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
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

fn classify_dep_source(spec: &str) -> String {
    if spec.starts_with("file:") || Path::new(spec).is_absolute() {
        "file".into()
    } else if spec.starts_with("git+")
        || spec.starts_with("github:")
        || spec.starts_with("git://")
        || spec.contains(".git")
    {
        "git".into()
    } else {
        "npm".into()
    }
}

fn composed_enabled(
    id: &str,
    layers: &[Vec<RowPatch>],
) -> (bool, bool) {
    let mut last: Option<bool> = None;
    let mut home_says = false;
    for (li, layer) in layers.iter().enumerate() {
        for p in layer.iter().filter(|p| p.id == id) {
            if let Some(d) = p.disabled {
                last = Some(d);
                if li == layers.len() - 1 {
                    home_says = true;
                }
            }
        }
    }
    (last.map(|d| !d).unwrap_or(true), home_says)
}

fn list_impl(runtime: &Path, home: &Path, app_data: &Path) -> Result<PluginsListData, String> {
    let profile = profile_dir(home);
    let mut data = PluginsListData {
        plugins: Vec::new(),
        service_restart_required: false,
        profile_dir: profile.display().to_string(),
        initialized: profile.join("package.json").exists(),
    };
    if !data.initialized {
        return Ok(data);
    }
    let manifest = read_manifest(&profile)?;
    let deps: Vec<(String, String)> = manifest
        .get("dependencies")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    let bundles: Vec<String> = manifest
        .pointer("/dsh/profile/bundles")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let snapshot = read_snapshot(app_data);

    let mut layers: Vec<Vec<RowPatch>> = Vec::new();
    for name in &bundles {
        let patches = resolve_bundle_dir(runtime, &profile, name)
            .map(|dir| bundle_row_patches(&dir))
            .unwrap_or_default();
        layers.push(patches);
    }
    let profile_patches = parse_patch_file_opt(&profile.join(PROFILE_PATCH_FILENAME))
        .unwrap_or_default();
    layers.push(row_patches(&profile_patches));
    let home_patches = parse_patch_file_opt(&home.join("cordis.patch.yml")).unwrap_or_default();
    layers.push(row_patches(&home_patches));

    for name in TEMPLATE_BUNDLES {
        let version = resolve_bundle_dir(runtime, &profile, &name.to_string())
            .and_then(|d| read_version(&d))
            .unwrap_or_else(|| "-".into());
        let restart_required = snapshot_differs(&snapshot, name, &version);
        data.plugins.push(PluginInfo {
            name: name.to_string(),
            version,
            spec: String::new(),
            source: "builtin".into(),
            builtin: true,
            is_bundle: true,
            rows: Vec::new(),
            restart_required,
        });
        if restart_required {
            data.service_restart_required = true;
        }
    }

    for (name, spec) in &deps {
        let is_bundle = exports_patch(runtime, &profile, name);
        let version = resolve_bundle_dir(runtime, &profile, name)
            .and_then(|d| read_version(&d))
            .unwrap_or_else(|| "-".into());
        let mut rows = Vec::new();
        if is_bundle {
            if let Some(dir) = resolve_bundle_dir(runtime, &profile, name) {
                let patches = bundle_row_patches(&dir);
                let mut seen = HashSet::new();
                for p in patches {
                    if !seen.insert(p.id.clone()) {
                        continue;
                    }
                    let (enabled, home_says) = composed_enabled(&p.id, &layers);
                    rows.push(PluginRow {
                        id: p.id,
                        name: p.name,
                        enabled,
                        managed: !home_says,
                        home_layer: home_says,
                    });
                }
            }
        }
        let restart_required = is_bundle && snapshot_differs(&snapshot, name, &version);
        if restart_required {
            data.service_restart_required = true;
        }
        data.plugins.push(PluginInfo {
            name: name.clone(),
            version,
            spec: spec.clone(),
            source: classify_dep_source(spec),
            builtin: false,
            is_bundle,
            rows,
            restart_required,
        });
    }
    Ok(data)
}

#[tauri::command]
pub fn plugins_list(state: State<'_, AppState>) -> Result<PluginsListData, String> {
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    list_impl(&runtime, &dsh_home(), &state.app_data_dir())
}

fn normalize_spec(spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("插件名不能为空".into());
    }
    let mut s = spec.to_string();
    if let Some(rest) = s.strip_prefix("file:") {
        s = rest.to_string();
    }
    if s == "."
        || s == ".."
        || s.starts_with("./")
        || s.starts_with(".\\")
        || s.starts_with("../")
        || s.starts_with("..\\")
    {
        return Err("本地路径必须是绝对路径（请粘贴完整路径）。".into());
    }
    if Path::new(&s).is_absolute() {
        if !Path::new(&s).exists() {
            return Err(format!("路径不存在：{s}"));
        }
        return Ok(s);
    }
    if s.starts_with("github:") || s.starts_with("git+") || s.starts_with("git://") {
        return Ok(s);
    }
    if s.contains('/') && !s.starts_with('@') {
        return Ok(format!("github:{}", s.trim_end_matches(".git")));
    }
    if s.ends_with(".git") {
        return Ok(s);
    }
    if s.contains(char::is_whitespace) {
        return Err("插件名不能包含空格。".into());
    }
    Ok(s)
}

fn spec_matches_name(spec: &str, dep_name: &str) -> bool {
    if spec == dep_name {
        return true;
    }
    if Path::new(spec).is_absolute() || spec.starts_with("file:") {
        let p = spec.strip_prefix("file:").unwrap_or(spec);
        return std::fs::read_to_string(Path::new(p).join("package.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .is_some_and(|n| n == dep_name);
    }
    false
}

fn add_impl(
    node: &Path,
    runtime: &Path,
    spec: &str,
    registry_source: &str,
) -> Result<PluginAddResult, String> {
    let _guard = PLUGIN_OPS.lock().unwrap();
    let home = dsh_home();
    let profile = profile_dir(&home);
    ensure_profile(&profile)?;
    let npm_spec = normalize_spec(spec)?;

    let before = read_manifest(&profile)?;
    let before_deps: HashSet<String> = dep_keys(&before).into_iter().collect();

    let mut args = vec![
        "install".to_string(),
        npm_spec.clone(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--no-update-notifier".into(),
        "--loglevel=error".into(),
        "--legacy-peer-deps".into(),
    ];
    args.extend(registry_flag(registry_source));

    let is_remote = !(npm_spec.starts_with("git+")
        || npm_spec.starts_with("github:")
        || npm_spec.starts_with("git://")
        || Path::new(&npm_spec).is_absolute());
    let timeout = if is_remote { NPM_TIMEOUT } else { GIT_TIMEOUT };

    match run_npm(node, &profile, &args, timeout) {
        Ok(()) => {}
        Err(first) => {
            if registry_source == "auto" && is_remote {
                log::warn!("npm install failed ({first}); retrying once with npmmirror");
                let mut retry = args.clone();
                retry.extend(["--registry".into(), "https://registry.npmmirror.com".into()]);
                run_npm(node, &profile, &retry, timeout)?;
            } else {
                return Err(first);
            }
        }
    }

    let after = read_manifest(&profile)?;
    let after_deps = dep_keys(&after);
    let new_deps: Vec<String> = after_deps
        .iter()
        .filter(|n| !before_deps.contains(*n))
        .cloned()
        .collect();
    let name = match new_deps.first() {
        Some(n) => n.clone(),
        None => {
            if dep_keys(&before).iter().any(|n| spec_matches_name(&npm_spec, n)) {
                return Err("该插件已安装。".into());
            }
            return Err("安装未生效（依赖未写入），请查看日志。".into());
        }
    };

    reconcile(&before_deps, runtime, &profile)?;

    let warning = if !exports_patch(runtime, &profile, &name) {
        Some("该包没有声明 dsh.bundle，不会作为功能层加载（仅作为普通依赖；其后续版本若带上 bundle 声明会自动生效）。".into())
    } else {
        None
    };
    let version = resolve_bundle_dir(runtime, &profile, &name)
        .and_then(|d| read_version(&d))
        .unwrap_or_else(|| "-".into());
    Ok(PluginAddResult {
        name,
        version,
        restart_required: true,
        warning,
    })
}

#[tauri::command]
pub async fn plugins_add(
    state: State<'_, AppState>,
    spec: String,
) -> Result<PluginAddResult, String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let registry_source = state.config.lock().unwrap().get().registry_source;
    tauri::async_runtime::spawn_blocking(move || add_impl(&node, &runtime, &spec, &registry_source))
        .await
        .map_err(|e| format!("安装任务异常: {e}"))?
}

pub fn removable_plugin_names(names: &[String]) -> Vec<String> {
    let profile = profile_dir(&dsh_home());
    removable_from_manifest(&profile, names)
}

fn removable_from_manifest(profile: &Path, names: &[String]) -> Vec<String> {
    let Ok(manifest) = read_manifest(profile) else {
        return Vec::new();
    };
    let deps = dep_keys(&manifest);
    deps.into_iter()
        .filter(|d| names.iter().any(|n| n == d))
        .collect()
}

fn remove_impl(node: &Path, runtime: &Path, name: &str, registry_source: &str) -> Result<(), String> {
    let _guard = PLUGIN_OPS.lock().unwrap();
    let profile = profile_dir(&dsh_home());
    let before = read_manifest(&profile)?;
    let before_deps: HashSet<String> = dep_keys(&before).into_iter().collect();
    if !before_deps.contains(name) {
        return Err("未安装该插件。".into());
    }
    let mut args = vec![
        "uninstall".to_string(),
        name.to_string(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--no-update-notifier".into(),
        "--loglevel=error".into(),
    ];
    args.extend(registry_flag(registry_source));
    run_npm(node, &profile, &args, NPM_TIMEOUT)?;
    reconcile(&before_deps, runtime, &profile)?;
    Ok(())
}

#[tauri::command]
pub async fn plugins_remove(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let registry_source = state.config.lock().unwrap().get().registry_source;
    tauri::async_runtime::spawn_blocking(move || remove_impl(&node, &runtime, &name, &registry_source))
        .await
        .map_err(|e| format!("卸载任务异常: {e}"))?
}

#[tauri::command]
pub async fn plugins_remove_incompatible(
    state: State<'_, AppState>,
    names: Vec<String>,
) -> Result<Vec<String>, String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let registry_source = state.config.lock().unwrap().get().registry_source;
    tauri::async_runtime::spawn_blocking(move || {
        let targets = removable_plugin_names(&names);
        let mut removed: Vec<String> = Vec::new();
        for name in &targets {
            match remove_impl(&node, &runtime, name, &registry_source) {
                Ok(()) => {
                    log::info!("plugins: removed incompatible plugin {name}");
                    removed.push(name.clone());
                }
                Err(e) => {
                    log::warn!("plugins: remove incompatible plugin {name} failed: {e}");
                }
            }
        }
        Ok(removed)
    })
    .await
    .map_err(|e| format!("移除任务异常: {e}"))?
}

pub fn ensure_default_plugin(node: &Path, runtime: &Path, app_data: &Path, registry_source: &str) {
    let marker = app_data.join(DEFAULT_PLUGIN_MARKER);
    if marker.exists() {
        return;
    }
    let home = dsh_home();
    let profile = profile_dir(&home);
    let already = read_manifest(&profile)
        .map(|m| dep_keys(&m).iter().any(|n| n == DEFAULT_PLUGIN))
        .unwrap_or(false);
    if already {
        if let Err(e) = std::fs::write(
            &marker,
            serde_json::json!({ "name": DEFAULT_PLUGIN, "installed": true }).to_string(),
        ) {
            log::warn!("plugins: write default-plugin marker failed: {e}");
        }
        return;
    }
    log::info!("plugins: auto-installing default plugin {DEFAULT_PLUGIN}");
    match add_impl(node, runtime, DEFAULT_PLUGIN, registry_source) {
        Ok(res) => {
            log::info!(
                "plugins: default plugin {} {} installed (restart required to activate)",
                res.name,
                res.version
            );
            if let Err(e) = std::fs::write(
                &marker,
                serde_json::json!({ "name": res.name, "version": res.version, "installed": true })
                    .to_string(),
            ) {
                log::warn!("plugins: write default-plugin marker failed: {e}");
            }
        }
        Err(e) => log::warn!("plugins: default plugin install failed (retry next launch): {e}"),
    }
}

fn set_enabled_impl(id: &str, enabled: bool) -> Result<(), String> {
    let _guard = PLUGIN_OPS.lock().unwrap();
    if id.trim().is_empty() {
        return Err("行 id 不能为空".into());
    }
    let profile = profile_dir(&dsh_home());
    ensure_profile(&profile)?;
    append_disable_row(&profile.join(PROFILE_PATCH_FILENAME), id.trim(), enabled)
}

#[tauri::command]
pub async fn plugins_set_enabled(id: String, enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_enabled_impl(&id, enabled))
        .await
        .map_err(|e| format!("切换任务异常: {e}"))?
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "dsh-desktop/0.1")
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

fn parse_awesome_readme(text: &str) -> Vec<MarketplaceItem> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("- [") else { continue };
        let Some(name_end) = rest.find("](") else { continue };
        let name = &rest[..name_end];
        let Some(url_end) = rest[name_end + 2..].find(')') else { continue };
        let url = &rest[name_end + 2..name_end + 2 + url_end];
        let desc = rest[name_end + 2 + url_end + 1..]
            .trim()
            .trim_start_matches('-')
            .trim_start_matches('—')
            .trim_start_matches('–')
            .trim();
        let trimmed_desc = desc.chars().take(160).collect::<String>();
        if let Some(pos) = url.find("npmjs.com/package/") {
            let pkg = url[pos + "npmjs.com/package/".len()..].replace("%2F", "/").replace("%2f", "/");
            out.push(MarketplaceItem {
                name: if name.is_empty() { pkg.clone() } else { name.to_string() },
                description: trimmed_desc,
                source: "npm".into(),
                spec: pkg,
                url: url.to_string(),
            });
        } else if let Some(pos) = url.find("github.com/") {
            let segs: Vec<&str> = url[pos + "github.com/".len()..]
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            if segs.len() >= 2 {
                let owner = segs[0];
                let repo = segs[1].trim_end_matches(".git");
                out.push(MarketplaceItem {
                    name: if name.is_empty() { repo.to_string() } else { name.to_string() },
                    description: trimmed_desc,
                    source: "github".into(),
                    spec: format!("github:{owner}/{repo}"),
                    url: format!("https://github.com/{owner}/{repo}"),
                });
            }
        }
    }
    out
}

#[derive(serde::Deserialize)]
struct GhSearchResponse {
    items: Vec<GhRepo>,
}

#[derive(serde::Deserialize)]
struct GhRepo {
    full_name: String,
    description: Option<String>,
    html_url: String,
}

async fn fetch_github_topic(client: &reqwest::Client) -> Result<Vec<MarketplaceItem>, String> {
    let url = "https://api.github.com/search/repositories?q=topic:dsh-plugin&sort=updated&per_page=30";
    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "dsh-desktop/0.1")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("GitHub 搜索失败：{e}"))?;
    match resp.status() {
        reqwest::StatusCode::OK => {
            let parsed: GhSearchResponse = resp
                .json()
                .await
                .map_err(|e| format!("GitHub 搜索返回异常：{e}"))?;
            Ok(parsed
                .items
                .into_iter()
                .filter_map(|r| {
                    let repo = r.full_name.rsplit('/').next()?.to_string();
                    Some(MarketplaceItem {
                        name: repo,
                        description: r.description.unwrap_or_default(),
                        source: "github".into(),
                        spec: format!("github:{}", r.full_name),
                        url: r.html_url,
                    })
                })
                .collect())
        }
        reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::TOO_MANY_REQUESTS => {
            Err("GitHub 搜索暂时受限（免费额度用完），稍后再试。".into())
        }
        s => Err(format!("GitHub 搜索失败（HTTP {s}）。")),
    }
}

#[tauri::command]
pub async fn plugins_marketplace(search: Option<String>) -> Result<MarketplaceResult, String> {
    let client = reqwest::Client::new();
    let mut items: Vec<MarketplaceItem> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let mut fetched = false;
    'outer: for branch in ["master", "main"] {
        for file in ["README.md", "README.zh.md"] {
            let url = format!(
                "https://raw.githubusercontent.com/awesome-dsh-plugin/awesome-dsh-plugin/{branch}/{file}"
            );
            if let Ok(text) = fetch_text(&client, &url).await {
                items.extend(parse_awesome_readme(&text));
                fetched = true;
                break 'outer;
            }
        }
    }
    if !fetched {
        errors.push("市场目录加载失败（网络不可达？）".into());
    }

    match fetch_github_topic(&client).await {
        Ok(list) => items.extend(list),
        Err(e) => errors.push(e),
    }

    let mut seen = HashSet::new();
    items.retain(|i| seen.insert(i.spec.clone()));
    if let Some(q) = search.as_deref().filter(|s| !s.trim().is_empty()) {
        let q = q.to_lowercase();
        items.retain(|i| {
            i.name.to_lowercase().contains(&q) || i.description.to_lowercase().contains(&q)
        });
    }
    items.truncate(100);
    Ok(MarketplaceResult { items, errors })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-plugins-{name}-{}", std::process::id()))
    }

    fn write_json(path: &Path, v: &serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(v).unwrap() + "\n").unwrap();
    }

    fn profile_with_deps(profile: &Path) {
        write_json(
            &profile.join("package.json"),
            &serde_json::json!({
                "name": "dsh-profile-web", "private": true,
                "dependencies": { "plugin-b": "^1.0.0", "plugin-a": "^1.0.0", "plugin-c": "^1.0.0" },
                "dsh": { "profile": { "bundles": [] } }
            }),
        );
    }

    #[test]
    fn removable_from_manifest_intersects_and_orders_deterministically() {
        let root = temp("removable");
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("profile");
        profile_with_deps(&profile);
        let names = vec![
            "plugin-c".to_string(),
            "ghost".to_string(),
            "plugin-a".to_string(),
            "plugin-b".to_string(),
        ];
        assert_eq!(
            removable_from_manifest(&profile, &names),
            vec!["plugin-a", "plugin-b", "plugin-c"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removable_from_manifest_empty_when_no_overlap() {
        let root = temp("removable-none");
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("profile");
        profile_with_deps(&profile);
        assert!(removable_from_manifest(&profile, &["ghost".to_string()]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removable_from_manifest_missing_profile_is_empty() {
        let root = temp("removable-missing");
        let _ = std::fs::remove_dir_all(&root);
        assert!(removable_from_manifest(&root.join("nope"), &["plugin-a".to_string()]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn make_runtime(root: &Path, bundle_name: &str, with_bundle: bool) {
        let dsh_dir = root.join("node_modules/@deepseek-ai/dsh");
        write_json(&dsh_dir.join("package.json"), &serde_json::json!({ "name": "@deepseek-ai/dsh", "version": "0.1.1-rc.2" }));
        if with_bundle {
            let dir = root.join("node_modules").join(bundle_name);
            write_json(
                &dir.join("package.json"),
                &serde_json::json!({ "name": bundle_name, "version": "1.0.0", "dsh": { "bundle": { "patch": "./cordis.patch.yml" } } }),
            );
            std::fs::write(
                dir.join("cordis.patch.yml"),
                "- insert:\n    - id: test-row\n      name: 'test-entry'\n",
            )
            .unwrap();
        }
    }

    #[test]
    fn resolve_bundle_dir_prefers_runtime_over_profile() {
        let root = temp("resolve");
        let _ = std::fs::remove_dir_all(&root);
        make_runtime(&root.join("runtime"), "@scope/pkg-a", true);
        let profile = root.join("profile");
        let dir = profile.join("node_modules/@scope/pkg-a");
        write_json(
            &dir.join("package.json"),
            &serde_json::json!({ "name": "@scope/pkg-a", "version": "9.9.9", "dsh": { "bundle": { "patch": "./cordis.patch.yml" } } }),
        );
        let resolved = resolve_bundle_dir(&root.join("runtime"), &profile, "@scope/pkg-a").unwrap();
        assert!(resolved.starts_with(root.join("runtime")), "runtime 锚点优先: {}", resolved.display());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_bundle_dir_falls_back_to_profile_and_none() {
        let root = temp("resolve-fallback");
        let _ = std::fs::remove_dir_all(&root);
        make_runtime(&root.join("runtime"), "unused", true);
        let profile = root.join("profile");
        let dir = profile.join("node_modules/only-in-profile");
        write_json(&dir.join("package.json"), &serde_json::json!({ "name": "only-in-profile", "version": "1.0.0" }));
        assert!(resolve_bundle_dir(&root.join("runtime"), &profile, "only-in-profile").is_some());
        assert!(resolve_bundle_dir(&root.join("runtime"), &profile, "ghost").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_registers_bundle_and_drops_removed() {
        let root = temp("reconcile");
        let _ = std::fs::remove_dir_all(&root);
        let runtime = root.join("runtime");
        make_runtime(&runtime, "plugin-a", true);
        let profile = root.join("profile");
        let plain = profile.join("node_modules/plain-b");
        write_json(&plain.join("package.json"), &serde_json::json!({ "name": "plain-b", "version": "1.0.0" }));
        write_json(
            &profile.join("package.json"),
            &serde_json::json!({
                "name": "dsh-profile-web", "private": true,
                "dependencies": { "plugin-a": "^1.0.0", "plain-b": "^1.0.0" },
                "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base"] } }
            }),
        );

        let before: HashSet<String> = HashSet::new();
        assert!(reconcile(&before, &runtime, &profile).unwrap());
        let manifest = read_manifest(&profile).unwrap();
        let bundles: Vec<String> = manifest
            .pointer("/dsh/profile/bundles")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        assert!(bundles.contains(&"plugin-a".to_string()), "bundle 依赖注册进层栈");
        assert!(!bundles.contains(&"plain-b".to_string()), "非 bundle 依赖不进层栈");
        assert!(bundles.contains(&"@deepseek-ai/dsh-base".to_string()), "内置 bundle 保留");

        write_json(
            &profile.join("package.json"),
            &serde_json::json!({
                "name": "dsh-profile-web", "private": true,
                "dependencies": {},
                "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "plugin-a"] } }
            }),
        );
        let before: HashSet<String> = ["plugin-a".to_string()].into_iter().collect();
        assert!(reconcile(&before, &runtime, &profile).unwrap());
        let manifest = read_manifest(&profile).unwrap();
        let bundles: Vec<String> = manifest
            .pointer("/dsh/profile/bundles")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        assert_eq!(bundles, vec!["@deepseek-ai/dsh-base".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_profile_creates_template_and_is_idempotent() {
        let root = temp("init");
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("web");
        ensure_profile(&profile).unwrap();
        let manifest = read_manifest(&profile).unwrap();
        assert_eq!(
            manifest.pointer("/dsh/profile/bundles/0").and_then(|v| v.as_str()),
            Some("@deepseek-ai/dsh-base")
        );
        assert_eq!(manifest.get("name").and_then(|v| v.as_str()), Some("dsh-profile-web"));
        assert!(profile.join("cordis.patch.yml").exists());
        assert!(profile.join("pnpm-workspace.yaml").exists());

        write_json(&profile.join("package.json"), &serde_json::json!({ "name": "mine" }));
        ensure_profile(&profile).unwrap();
        let manifest = read_manifest(&profile).unwrap();
        assert_eq!(manifest.get("name").and_then(|v| v.as_str()), Some("mine"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn append_disable_row_template_shape() {
        let root = temp("append");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let patch = root.join("cordis.patch.yml");
        std::fs::write(&patch, PROFILE_PATCH_TEMPLATE).unwrap();
        append_disable_row(&patch, "row-x", true).unwrap();
        append_disable_row(&patch, "row-x", false).unwrap();
        let text = std::fs::read_to_string(&patch).unwrap();
        assert!(text.contains("- id: row-x\n  disabled: true"));
        assert!(text.contains("- id: row-x\n  disabled: false"));
        let parsed: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        let seq = parsed.as_sequence().unwrap();
        assert_eq!(seq.len(), 2);
        let last_disabled = seq[1].get("disabled").and_then(|v| v.as_bool());
        assert_eq!(last_disabled, Some(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn append_disable_row_quotes_id_and_rejects_bad_yaml() {
        let root = temp("append-quote");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let patch = root.join("cordis.patch.yml");
        std::fs::write(&patch, "[]\n").unwrap();
        append_disable_row(&patch, "has: colon", true).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&patch).unwrap()).unwrap();
        let id = parsed.as_sequence().unwrap()[0].get("id").and_then(|v| v.as_str());
        assert_eq!(id, Some("has: colon"));

        std::fs::write(&patch, "{{{{").unwrap();
        assert!(append_disable_row(&patch, "x", true).is_err(), "坏 YAML 必须报修复错误");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn row_patches_tolerates_js_tags() {
        let root = temp("parse");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let patch = root.join("cordis.patch.yml");
        std::fs::write(
            &patch,
            "- insert:\n    - id: row-a\n      name: 'x'\n      config:\n        path: !!js dshHomePath('sessions')\n- id: row-b\n  disabled: true\n",
        )
        .unwrap();
        let entries = parse_patch_file_opt(&patch).unwrap();
        let rows = row_patches(&entries);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["row-a", "row-b"]);
        assert_eq!(rows[1].disabled, Some(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_spec_cases() {
        assert_eq!(normalize_spec("dsh-plugin-manager").unwrap(), "dsh-plugin-manager");
        assert_eq!(normalize_spec("@scope/pkg").unwrap(), "@scope/pkg");
        assert_eq!(normalize_spec("some-org/my-plugin").unwrap(), "github:some-org/my-plugin");
        assert_eq!(normalize_spec("some-org/my-plugin.git").unwrap(), "github:some-org/my-plugin");
        assert_eq!(normalize_spec("github:a/b").unwrap(), "github:a/b");
        assert_eq!(normalize_spec("git+https://example.com/x.git").unwrap(), "git+https://example.com/x.git");
        assert!(normalize_spec("./plugin").is_err());
        assert!(normalize_spec("../plugin").is_err());
        assert!(normalize_spec("").is_err());
        let root = temp("spec");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            normalize_spec(&root.to_string_lossy()).unwrap(),
            root.to_string_lossy().to_string()
        );
        assert!(normalize_spec(&root.join("ghost").to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_default_plugin_marks_when_already_installed() {
        let root = temp("auto-marker");
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let profile = profile_dir(&home);
        ensure_profile(&profile).unwrap();
        let mut manifest = read_manifest(&profile).unwrap();
        manifest["dependencies"] = serde_json::json!({ "dshmarket": "^1.0.0" });
        write_manifest(&profile, &manifest).unwrap();
        let app_data = root.join("appdata");
        std::fs::create_dir_all(&app_data).unwrap();
        ensure_default_plugin(&PathBuf::new(), &PathBuf::new(), &app_data, "auto");
        assert!(app_data.join(DEFAULT_PLUGIN_MARKER).exists(), "补写标记");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_default_plugin_skips_when_marker_exists() {
        let root = temp("auto-skip");
        let _ = std::fs::remove_dir_all(&root);
        let app_data = root.join("appdata");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::write(app_data.join(DEFAULT_PLUGIN_MARKER), "{\"installed\":true}").unwrap();
        ensure_default_plugin(&PathBuf::new(), &PathBuf::new(), &app_data, "auto");
        assert!(!profile_dir(&root.join("home")).exists(), "标记短路，无副作用");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_awesome_readme_classifies_entries() {
        let text = "# Awesome\n- [My Plugin](https://github.com/someone/my-plugin)\n- [@scope/cool](https://www.npmjs.com/package/@scope/cool)\n- [skip me](https://example.com/nope)\n";
        let items = parse_awesome_readme(text);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].spec, "github:someone/my-plugin");
        assert_eq!(items[0].source, "github");
        assert_eq!(items[1].spec, "@scope/cool");
        assert_eq!(items[1].source, "npm");
    }
}
