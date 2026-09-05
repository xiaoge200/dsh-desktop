use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) const PROFILE_NAME: &str = "web";
pub(crate) const PROFILE_PATCH_FILENAME: &str = "cordis.patch.yml";
pub(crate) const TEMPLATE_BUNDLES: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];

const PROFILE_PATCH_TEMPLATE: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n\
# a top-level YAML array of loader patch entries (id-targeted config\n\
# overrides, disables, and insert lists; `!!js` expressions allowed).\n\
[]\n";

const PROFILE_PNPM_WORKSPACE: &str = "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n";

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

pub(crate) fn dsh_home() -> PathBuf {
    let configured = std::env::var("DSH_HOME").ok().filter(|v| !v.trim().is_empty());
    match configured {
        Some(p) => make_absolute(expand_tilde(&p)),
        None => dirs::home_dir()
            .map(|h| h.join(".dsh"))
            .unwrap_or_else(|| PathBuf::from(".dsh")),
    }
}

pub(crate) fn profile_dir(home: &Path) -> PathBuf {
    home.join("profiles").join(PROFILE_NAME)
}

pub(crate) fn ensure_profile(dir: &Path) -> Result<(), String> {
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

pub(crate) fn read_manifest(dir: &Path) -> Result<serde_json::Value, String> {
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

pub(crate) fn write_manifest(dir: &Path, manifest: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())? + "\n";
    std::fs::write(dir.join("package.json"), text).map_err(|e| format!("写入 profile 配置失败: {e}"))
}

pub(crate) fn dep_keys(manifest: &serde_json::Value) -> Vec<String> {
    manifest
        .get("dependencies")
        .and_then(|v| v.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

pub(crate) fn read_version(dir: &Path) -> Option<String> {
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

pub(crate) fn resolve_bundle_dir(runtime: &Path, profile: &Path, package_name: &str) -> Option<PathBuf> {
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

pub(crate) fn exports_patch(runtime: &Path, profile: &Path, package_name: &str) -> bool {
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

pub(crate) fn reconcile(
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

pub(crate) struct RowPatch {
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) disabled: Option<bool>,
}

fn yaml_get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

pub(crate) fn row_patches(entries: &[serde_yaml::Value]) -> Vec<RowPatch> {
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

pub(crate) fn parse_patch_file_opt(path: &Path) -> Option<Vec<serde_yaml::Value>> {
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

pub(crate) fn bundle_row_patches(dir: &Path) -> Vec<RowPatch> {
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

pub(crate) fn append_disable_row(patch_path: &Path, id: &str, enabled: bool) -> Result<(), String> {
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
}
