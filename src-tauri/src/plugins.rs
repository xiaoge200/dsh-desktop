pub(crate) mod plugin_market;
mod plugin_npm;
mod plugin_profile;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use plugin_npm::{registry_flag, run_npm, GIT_TIMEOUT, NPM_TIMEOUT};
pub(crate) use plugin_profile::dsh_home;
use plugin_profile::{
    append_disable_row, bundle_row_patches, dep_keys, ensure_profile, exports_patch,
    parse_patch_file_opt, profile_dir, read_manifest, read_version, reconcile, resolve_bundle_dir,
    row_patches, RowPatch, PROFILE_NAME, PROFILE_PATCH_FILENAME, TEMPLATE_BUNDLES,
};
use crate::service::{ops_guard, run_exclusive_mutation};
use crate::state::AppState;

const SNAPSHOT_FILE: &str = "plugins-state.json";
const DEFAULT_PLUGIN: &str = "dshmarket";
const DEFAULT_PLUGIN_MARKER: &str = "default-plugin.json";

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
    if crate::service::OPS_LOCK.try_lock().is_err() {
        log::warn!("plugins: snapshot skipped (plugin op in progress)");
        return;
    }
    write_snapshot(state);
}

// 调用方必须已持 OPS_LOCK（run_exclusive_mutation 成功路径在锁内调用）。
pub fn record_restart_force(state: &AppState) {
    write_snapshot(state);
}

fn write_snapshot(state: &AppState) {
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
            name: name.to_string(),
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
pub async fn plugins_list(app: AppHandle) -> Result<PluginsListData, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let runtime = state.runtime_dir();
        if runtime.as_os_str().is_empty() {
            return Err("服务尚未初始化".into());
        }
        list_impl(&runtime, &dsh_home(), &state.app_data_dir())
    })
    .await
    .map_err(|e| format!("列表任务异常: {e}"))?
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
    app: AppHandle,
    state: State<'_, AppState>,
    spec: String,
) -> Result<PluginAddResult, String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let registry_source = state.config.lock().unwrap().get().registry_source;
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let deps = read_manifest(&profile_dir(&dsh_home()))
            .map(|m| dep_keys(&m))
            .unwrap_or_default();
        if deps
            .iter()
            .any(|n| n == &spec || spec_matches_name(&spec, n))
        {
            return Err("该插件已安装。".into());
        }
        run_exclusive_mutation(&app2, &state, "plugin add", move || {
            add_impl(&node, &runtime, &spec, &registry_source)
        })
    })
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
pub async fn plugins_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<(), String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let registry_source = state.config.lock().unwrap().get().registry_source;
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let deps = read_manifest(&profile_dir(&dsh_home()))
            .map(|m| dep_keys(&m))
            .unwrap_or_default();
        if !deps.contains(&name) {
            return Err("未安装该插件。".into());
        }
        run_exclusive_mutation(&app2, &state, "plugin remove", move || {
            remove_impl(&node, &runtime, &name, &registry_source)
        })
    })
    .await
    .map_err(|e| format!("卸载任务异常: {e}"))?
}

#[tauri::command]
pub async fn plugins_remove_incompatible(
    app: AppHandle,
    state: State<'_, AppState>,
    names: Vec<String>,
) -> Result<Vec<String>, String> {
    let node = state.node_path();
    let runtime = state.runtime_dir();
    if runtime.as_os_str().is_empty() {
        return Err("服务尚未初始化".into());
    }
    let registry_source = state.config.lock().unwrap().get().registry_source;
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app2.state::<AppState>();
        let targets = removable_plugin_names(&names);
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        run_exclusive_mutation(&app2, &state, "plugin remove-incompatible", move || {
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
    if id.trim().is_empty() {
        return Err("行 id 不能为空".into());
    }
    let profile = profile_dir(&dsh_home());
    ensure_profile(&profile)?;
    append_disable_row(&profile.join(PROFILE_PATCH_FILENAME), id.trim(), enabled)
}

#[tauri::command]
pub async fn plugins_set_enabled(id: String, enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = ops_guard();
        set_enabled_impl(&id, enabled)
    })
    .await
    .map_err(|e| format!("切换任务异常: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::plugin_profile::write_manifest;

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
}
