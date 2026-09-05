use std::collections::HashSet;
use std::time::Duration;

use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct MarketplaceItem {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) source: String,
    pub(crate) spec: String,
    pub(crate) url: String,
}

#[derive(Serialize)]
pub(crate) struct MarketplaceResult {
    pub(crate) items: Vec<MarketplaceItem>,
    pub(crate) errors: Vec<String>,
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
pub(crate) async fn plugins_marketplace(search: Option<String>) -> Result<MarketplaceResult, String> {
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
