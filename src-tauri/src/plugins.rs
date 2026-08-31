//! 插件管理（FR-18）：安装 / 移除 / 启停 DeepSeek Harness 插件 + 应用市场。
//!
//! 设计原则：不另起炉灶，直接复用 dsh 自带的 Cordis 插件体系。
//! - 插件实体 = 声明了 `"dsh": { "bundle": { "patch": ... } }` 的 npm 包（bundle），
//!   安装在 `$DSH_HOME/profiles/web`（桌面端运行的 `web` profile，dsh 运行时更新
//!   不会动这里，天然持久）。
//! - 安装后端用内置 Node 自带的 npm（`npm-cli.js`），并复刻上游
//!   `dsh plugin` 的 `reconcilePlugins`（bundle 注册进 `dsh.profile.bundles`）
//!   与 `initProfile`（profile 模板初始化）——语义与
//!   `@deepseek-ai/dsh/lib/plugin-*.js` / `@deepseek-ai/dsh-app-boot` 完全一致。
//! - `--legacy-peer-deps` 对齐上游 pnpm 的 `autoInstallPeers: false`：插件依赖的
//!   同名前缀包从 dsh 安装树解析，避免 npm 把 peer 树复制进 profile 造成遮蔽。
//! - 启用/禁用 = 在 profile 的 `cordis.patch.yml` 追加 `- id: <id> disabled: <bool>`
//!   行（后写覆盖先写）。dsh 的 `watchUserPatches` 热重载，无需重启。
//! - 新增/移除 bundle 只在下一次服务启动时读取 → 用「重启快照」对比最近一次
//!   就绪时的 bundle 集合，向前端提示需要重启。
//! - 补丁文件用 `serde_yaml::Value` 解析，容忍 `!!js` 自定义标签；且只做
//!   文本追加、从不整文件重序列化用户补丁（注释与标签原样保留）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// 桌面端运行/管理的 profile（dsh web 的别名 profile）
const PROFILE_NAME: &str = "web";
/// profile 内的用户补丁层文件名（dsh-app-boot 的 PROFILE_PATCH_FILENAME）
const PROFILE_PATCH_FILENAME: &str = "cordis.patch.yml";
/// 重启快照（appData 下，壳自己维护）
const SNAPSHOT_FILE: &str = "plugins-state.json";
/// 默认自动安装的插件：dsh-market 市场插件（npm 包名 dshmarket），
/// 装进 dsh 网页设置里的插件市场，桌面端无需自建市场数据源
const DEFAULT_PLUGIN: &str = "dshmarket";
/// 默认插件安装标记（appData 下）：安装成功后写入——用户之后手动移除
/// 不会被自动装回；失败不写，下次启动重试
const DEFAULT_PLUGIN_MARKER: &str = "default-plugin.json";

/// 上游 PROFILE_TEMPLATES.web（dsh-app-boot/lib/index.js:323）
const TEMPLATE_BUNDLES: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];

/// 上游 PROFILE_PATCH_TEMPLATE（dsh-app-boot/lib/index.js:335，逐字复制）
const PROFILE_PATCH_TEMPLATE: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n\
# a top-level YAML array of loader patch entries (id-targeted config\n\
# overrides, disables, and insert lists; `!!js` expressions allowed).\n\
[]\n";

/// 上游 PROFILE_PNPM_WORKSPACE（dsh-app-boot/lib/index.js:340，逐字复制；
/// npm 不读它，保留是为了与 CLI 手工管理时 pnpm 的行为一致）
const PROFILE_PNPM_WORKSPACE: &str = "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n";

/// 远端（registry）安装超时；git/本地路径安装超时（prepare 脚本可能很慢）
const NPM_TIMEOUT: Duration = Duration::from_secs(600);
const GIT_TIMEOUT: Duration = Duration::from_secs(1800);

/// 插件操作的全局串行锁（安装/卸载/启停/快照互斥；与 supervisor 锁无关）
static PLUGIN_OPS: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// 前端数据结构
// ---------------------------------------------------------------------------

/// 插件的某一行（patch row）的启用状态
#[derive(Serialize)]
pub struct PluginRow {
    /// 行 id（cordis patch 行的标识）
    pub id: String,
    /// 行声明的入口名（模块 spec，可能为空）
    pub name: Option<String>,
    /// 组合后的启用状态（最后一层 disabled 取反）
    pub enabled: bool,
    /// 是否可以在设置页切换（被 home 层禁用时不可切换）
    pub managed: bool,
    /// 是否被 home 层（$DSH_HOME/cordis.patch.yml）覆盖（优先级高于 profile 层）
    pub home_layer: bool,
}

/// 一个已安装插件（内置 bundle 或用户依赖）
#[derive(Serialize)]
pub struct PluginInfo {
    /// 依赖名（npm 安装写入的真实包名）
    pub name: String,
    /// 解析出的版本（无法解析时为 "-"）
    pub version: String,
    /// profile 依赖里的原始 spec
    pub spec: String,
    /// "builtin" | "npm" | "git" | "file"
    pub source: String,
    /// 内置模板 bundle（只读展示，不可移除/不展开行）
    pub builtin: bool,
    /// 是否声明了 dsh.bundle（会作为功能层加载）
    pub is_bundle: bool,
    /// bundle 补丁里声明的行（含启用状态；内置 bundle 为空）
    pub rows: Vec<PluginRow>,
    /// 需要重启服务后生效（相对最近一次就绪快照有变化）
    pub restart_required: bool,
}

#[derive(Serialize)]
pub struct PluginsListData {
    pub plugins: Vec<PluginInfo>,
    /// 任一插件需要重启（前端显示总提示条）
    pub service_restart_required: bool,
    pub profile_dir: String,
    /// profile 是否已初始化（首次安装时自动创建）
    pub initialized: bool,
}

#[derive(Serialize)]
pub struct PluginAddResult {
    pub name: String,
    pub version: String,
    /// 新 bundle 行只在启动时读取 → 恒为 true
    pub restart_required: bool,
    /// 该包未声明 dsh.bundle 时的提示（不是错误）
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct MarketplaceItem {
    pub name: String,
    pub description: String,
    /// "npm" | "github"
    pub source: String,
    /// 可直接传给 plugins_add 的 spec
    pub spec: String,
    pub url: String,
}

#[derive(Serialize)]
pub struct MarketplaceResult {
    pub items: Vec<MarketplaceItem>,
    /// 非致命错误（限流/网络），列表仍可用
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// 路径与 profile 初始化（复刻 dsh-home-paths / dsh-app-boot 的 initProfile）
// ---------------------------------------------------------------------------

/// 展开 `~` / `~/` / `~\` 前缀（复刻 dsh-home-paths 的 expandHomePath）
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

/// 解析 Harness 主目录（复刻 dsh-home-paths 的 resolveDshHome）：
/// `$DSH_HOME`（非空）优先，否则 `~/.dsh`。
pub fn dsh_home() -> PathBuf {
    let configured = std::env::var("DSH_HOME").ok().filter(|v| !v.trim().is_empty());
    match configured {
        Some(p) => make_absolute(expand_tilde(&p)),
        None => dirs::home_dir()
            .map(|h| h.join(".dsh"))
            .unwrap_or_else(|| PathBuf::from(".dsh")),
    }
}

/// profile 目录：`$DSH_HOME/profiles/web`
pub fn profile_dir(home: &Path) -> PathBuf {
    home.join("profiles").join(PROFILE_NAME)
}

/// 初始化 profile 目录（复刻 dsh-app-boot 的 initProfile：已存在的文件绝不覆盖）
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

// ---------------------------------------------------------------------------
// bundle 解析与注册（复刻 dsh-app-boot 的 packageDirFromAnchor / resolveBundleDir
// 与 dsh/lib/plugin-*.js 的 exportsPatch / reconcilePlugins）
// ---------------------------------------------------------------------------

/// 从锚点文件出发的 Node 模块查找：逐级向上探测 `<d>/node_modules/<name>`。
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

/// 解析 bundle 包目录：先 dsh 安装树，再 profile 目录（上游的安装锚点优先契约）。
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

/// 依赖是否声明了 `dsh.bundle.patch`（即是一个 bundle 插件）。
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

/// 复刻上游 reconcilePlugins：把声明了 dsh.bundle 的依赖注册进
/// `dsh.profile.bundles`；被移除或不再声明 bundle 的依赖退出层栈。
/// 内置模板 bundle 不在依赖里，永不触碰。返回是否有变化。
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
            // 与上游一致的提示（仅提示，不是错误）
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

// ---------------------------------------------------------------------------
// 补丁文件（cordis.patch.yml）解析与追加
// ---------------------------------------------------------------------------

/// 一行的补丁状态（某个层对该行的声明）
struct RowPatch {
    id: String,
    name: Option<String>,
    disabled: Option<bool>,
}

fn yaml_get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

/// 解析一个补丁层为「行声明」序列（直接 id 行与 insert 列表中的行）。
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

/// 解析补丁文件（容忍 `!!js` 标签——serde_yaml 会保成 TaggedValue，不会失败）。
/// 失败时返回 None 并记日志（列表页降级展示，启停操作则给出修复提示）。
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

/// bundle 补丁里声明的行（入口 spec + 是否显式 disabled）
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

/// 在补丁文件末尾追加一行 `- id: <id>\n  disabled: <bool>`（后写覆盖先写）。
/// 只做文本追加，从不整文件重序列化（保留用户注释与 `!!js` 表达式）；
/// 罕见流式写法才回退到整文件重写并记日志。
fn append_disable_row(patch_path: &Path, id: &str, enabled: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(patch_path)
        .map_err(|e| format!("读取补丁文件失败（{}）: {e}", patch_path.display()))?;
    let parsed: serde_yaml::Value = serde_yaml::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| "cordis.patch.yml 无法解析（可能被手动改坏），请修复后重试。".to_string())?;
    if !matches!(parsed, serde_yaml::Value::Sequence(_)) {
        return Err("cordis.patch.yml 必须是顶层 YAML 数组（补丁条目列表）。".into());
    }

    // 用 serde_yaml 序列化单行，保证 id 的引号/转义正确
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

    // YAML 块序列没有收尾符：空流式 `[]`（模板形态）就地展开为块式条目；
    // 块式文件直接追加；其余流式写法整文件重写（注释/!!js 标签可能丢失）。
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

    // 原子写（与 dsh Include._writeFile 同思路：tmp + rename）
    let tmp = patch_path.with_extension("yml.tmp");
    std::fs::write(&tmp, &new_text).map_err(|e| format!("写入补丁文件失败: {e}"))?;
    std::fs::rename(&tmp, patch_path).map_err(|e| format!("替换补丁文件失败: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 重启快照（最近一次服务就绪时的 bundle 集合；用于判断是否需要重启）
// ---------------------------------------------------------------------------

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

/// bundle 是否相对快照缺失或版本变化（需要重启）
fn snapshot_differs(snap: &Option<Snapshot>, name: &str, version: &str) -> bool {
    let Some(s) = snap else { return false };
    match s.bundles.iter().find(|b| b.name == name) {
        None => true,
        Some(b) => version != "-" && b.version.as_deref() != Some(version),
    }
}

/// 服务就绪后记录快照（boot / restart_service / repair_service 成功后调用）。
/// 不阻塞调用方：拿不到锁或写盘失败都只是少一次提示，下次重启会修正。
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

// ---------------------------------------------------------------------------
// npm 调用（内置 Node 的 npm-cli.js，不依赖系统 PATH）
// ---------------------------------------------------------------------------

/// 复刻 install-dsh.mjs 的 resolveNpmCli 三候选布局
fn npm_cli_path(node: &Path) -> Option<PathBuf> {
    let node_dir = node.parent()?;
    let candidates = [
        // Windows 官方发行版：<nodeDir>/node_modules/npm/bin/npm-cli.js
        node_dir.join("node_modules").join("npm").join("bin").join("npm-cli.js"),
        // mac/Linux 官方发行版（bin 在 nodeDir）：<nodeDir>/../lib/node_modules/npm
        node_dir.join("..").join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"),
        // 本项目资源布局：node 二进制与 lib 平级
        node_dir.join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// 根据用户选择的更新源构造 registry 参数（与 node.rs 的 registry_args 同语义）
fn registry_flag(source: &str) -> Vec<String> {
    match source {
        "npmjs" => vec!["--registry".into(), "https://registry.npmjs.org".into()],
        "npmmirror" => vec!["--registry".into(), "https://registry.npmmirror.com".into()],
        // auto 或未知：不传，npm 默认源（尊重用户级 .npmrc）
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

/// 把 npm 的报错提炼成可读的几句（npm 9+ 用 "npm error"，旧版用 "npm ERR!"）
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

/// 用内置 Node 运行 npm-cli.js（cwd = profile 目录），带超时杀树。
fn run_npm(node: &Path, cwd: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
    let npm_cli = npm_cli_path(node).ok_or("内置 npm 不完整，请重新安装本应用。")?;
    let node_n = crate::node::normalize_for_node(node);
    let npm_n = crate::node::normalize_for_node(&npm_cli);
    let cwd_n = crate::node::normalize_for_node(cwd);
    let mut full_args: Vec<String> = vec![npm_n.to_string_lossy().to_string()];
    full_args.extend(args.iter().cloned());
    log::info!("npm: {:?} {:?}", node_n, full_args);

    // 平台相关启动：Windows 用「隐藏控制台」+ 匿名管道（npm 及其派生的 cmd/node
    // 子进程都不弹窗口）；Unix 用 std Command + 进程组便于整组清理。
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
        // 父进程关闭写端：子进程退出后读端才能 EOF
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

    // 边跑边排空管道，避免 npm 输出撑满缓冲区卡死
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
                        Err(_) => break, // 超时或管道关闭：读取线程已结束
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
            // 超时：杀整棵树（与 Supervisor::stop 同策略）
            let pid = child.id();
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // CREATE_NO_WINDOW：避免超时杀进程时闪黑窗口
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

// ---------------------------------------------------------------------------
// 插件列表
// ---------------------------------------------------------------------------

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

/// 计算某行的组合启用状态：各层（bundle 层按顺序 → profile 层 → home 层）
/// 最后一个带 disabled 的声明生效（后写覆盖先写，与 applyEntryPatches 一致）。
fn composed_enabled(
    id: &str,
    layers: &[Vec<RowPatch>],
) -> (bool, bool) {
    // (enabled, home_says) —— home 层声明过 disabled 时 profile 层无法切换
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

    // 组合层：所有 bundle 补丁（按 bundles 顺序）→ profile 层 → home 层
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

    // 内置模板 bundle：只读展示（随 DSH 运行时更新，不展开行、不可移除）
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

    // 用户依赖
    for (name, spec) in &deps {
        let is_bundle = exports_patch(runtime, &profile, name);
        let version = resolve_bundle_dir(runtime, &profile, name)
            .and_then(|d| read_version(&d))
            .unwrap_or_else(|| "-".into());
        let mut rows = Vec::new();
        if is_bundle {
            if let Some(dir) = resolve_bundle_dir(runtime, &profile, name) {
                let patches = bundle_row_patches(&dir);
                // 去重保序
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

// ---------------------------------------------------------------------------
// 安装 / 移除
// ---------------------------------------------------------------------------

/// 规格化用户输入的 spec（npm 包名 / github:owner/repo / 本地绝对路径）
fn normalize_spec(spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("插件名不能为空".into());
    }
    let mut s = spec.to_string();
    if let Some(rest) = s.strip_prefix("file:") {
        s = rest.to_string();
    }
    // 相对路径：桌面端没有"调用目录"语义，一律要求绝对路径
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
        // owner/repo 简写 → github:（与市场条目生成的 spec 一致；带 .git 后缀也归一化）
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

/// 依赖名与用户输入的 spec 是否对应（用于"已安装"提示）
fn spec_matches_name(spec: &str, dep_name: &str) -> bool {
    if spec == dep_name {
        return true;
    }
    // file: 形式 → 读目录内 package.json 的真实名
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
        // 对齐上游 pnpm 的 autoInstallPeers: false——插件依赖的 @deepseek-ai/* peer
        // 由 dsh 安装树的 fallback 符号链接提供，npm 不得自行复制进 profile
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
            // auto 模式网络失败兜底：换国内镜像重试一次（仅远端包名）
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

/// 安装插件（异步命令：npm 可能耗时数分钟，放线程池执行避免卡界面）
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

/// 卸载插件（异步命令，与安装同理）
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

/// 后台确保默认插件（dshmarket 市场插件）已安装。幂等：
/// - 标记文件已存在 → 直接返回（用户已移除过也不会再装回）
/// - 用户之前手动装过 → 只补写标记
/// - 否则走 add_impl 安装（与手动安装同一条链路）；失败静默记日志，下次启动重试
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

// ---------------------------------------------------------------------------
// 启用 / 禁用（热生效，无需重启）
// ---------------------------------------------------------------------------

fn set_enabled_impl(id: &str, enabled: bool) -> Result<(), String> {
    let _guard = PLUGIN_OPS.lock().unwrap();
    if id.trim().is_empty() {
        return Err("行 id 不能为空".into());
    }
    let profile = profile_dir(&dsh_home());
    ensure_profile(&profile)?;
    append_disable_row(&profile.join(PROFILE_PATCH_FILENAME), id.trim(), enabled)
}

/// 启用/禁用某一行（追加 profile 补丁行；dsh 热重载，立即生效）
#[tauri::command]
pub async fn plugins_set_enabled(id: String, enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_enabled_impl(&id, enabled))
        .await
        .map_err(|e| format!("切换任务异常: {e}"))?
}

// ---------------------------------------------------------------------------
// 应用市场
// ---------------------------------------------------------------------------

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

/// 解析 awesome 目录 README 的 markdown 链接行：`- [name](url) - 描述`
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

/// GitHub `dsh-plugin` 主题搜索（次源；API 有免费额度，失败不致命）
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

/// 应用市场：awesome 目录（主源，raw.githubusercontent 不走 API 限流）+
/// GitHub 主题搜索（次源）。search 为空返回全部，否则本地过滤。
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

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

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

    /// 造一个 runtime 骨架（dsh 安装锚点 + 一个 bundle）
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
        // profile 里也放一份同名包（应被安装树优先解析）
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
        // 非 bundle 依赖
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

        // 移除依赖后：plugin-a 应退出层栈
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

        // 幂等：已有文件绝不覆盖
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
        // 仍是合法补丁：顶层数组，两个条目（后写覆盖先写）
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
        // npm 名
        assert_eq!(normalize_spec("dsh-plugin-manager").unwrap(), "dsh-plugin-manager");
        assert_eq!(normalize_spec("@scope/pkg").unwrap(), "@scope/pkg");
        // owner/repo 简写 → github:
        assert_eq!(normalize_spec("some-org/my-plugin").unwrap(), "github:some-org/my-plugin");
        assert_eq!(normalize_spec("some-org/my-plugin.git").unwrap(), "github:some-org/my-plugin");
        // git 显式形式原样通过
        assert_eq!(normalize_spec("github:a/b").unwrap(), "github:a/b");
        assert_eq!(normalize_spec("git+https://example.com/x.git").unwrap(), "git+https://example.com/x.git");
        // 相对路径拒绝
        assert!(normalize_spec("./plugin").is_err());
        assert!(normalize_spec("../plugin").is_err());
        assert!(normalize_spec("").is_err());
        // 绝对路径要求存在
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
        // 模拟"用户之前手动装过 dshmarket"：写进依赖即可
        let mut manifest = read_manifest(&profile).unwrap();
        manifest["dependencies"] = serde_json::json!({ "dshmarket": "^1.0.0" });
        write_manifest(&profile, &manifest).unwrap();
        let app_data = root.join("appdata");
        std::fs::create_dir_all(&app_data).unwrap();
        // node/runtime 为空也没关系：已装路径不触发 npm
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
        // 标记存在 → 完全短路：profile 都不该被创建
        ensure_default_plugin(&PathBuf::new(), &PathBuf::new(), &app_data, "auto");
        assert!(!profile_dir(&root.join("home")).exists(), "标记短路，无副作用");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_awesome_readme_classifies_entries() {
        let text = "\
# Awesome\n\
- [My Plugin](https://github.com/someone/my-plugin) — does things\n\
- [@scope/cool](https://www.npmjs.com/package/@scope%2Fcool)\n\
- [skip me](https://example.com/other) — not a repo\n";
        let items = parse_awesome_readme(text);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].spec, "github:someone/my-plugin");
        assert_eq!(items[0].source, "github");
        assert_eq!(items[1].spec, "@scope/cool");
        assert_eq!(items[1].source, "npm");
    }
}
