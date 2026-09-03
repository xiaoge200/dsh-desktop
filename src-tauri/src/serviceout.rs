use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    PluginTree,
    StaleLock { lock_path: Option<String> },
    Unknown,
}

impl FailureKind {
    pub fn kind_str(&self) -> &'static str {
        match self {
            FailureKind::PluginTree => "plugins",
            FailureKind::StaleLock { .. } => "stale-lock",
            FailureKind::Unknown => "unknown",
        }
    }
}

const PLUGIN_MARKERS: [&str; 3] = [
    "plugin tree failed to load",
    "failed to import loader entry",
    "does not provide an export named",
];
const LOCK_MARKERS: [&str; 2] = [
    "leftover from an unclean shutdown",
    "remove it manually and retry",
];
const LOCK_PATH_MARK: &str = "unreadable: ";

const PRUNE_CHUNK: usize = 4096;
pub const DEFAULT_LIMIT: usize = 128 * 1024;

pub struct ServiceCapture {
    inner: Mutex<CaptureInner>,
}

struct CaptureInner {
    buf: Vec<u8>,
    limit: usize,
}

impl ServiceCapture {
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Mutex::new(CaptureInner {
                buf: Vec::new(),
                limit,
            }),
        }
    }

    pub fn push_bytes(&self, chunk: &[u8]) {
        let mut g = self.inner.lock().unwrap();
        if g.limit == 0 {
            return;
        }
        g.buf.extend_from_slice(chunk);
        while g.buf.len() > g.limit {
            let cut = (g.buf.len() - g.limit).min(PRUNE_CHUNK);
            g.buf.drain(..cut);
        }
    }

    pub fn text(&self) -> String {
        let g = self.inner.lock().unwrap();
        String::from_utf8_lossy(&g.buf).into_owned()
    }

    pub fn captured_url(&self, port: u16) -> Option<String> {
        let text = self.text();
        if port == 0 {
            return None;
        }
        extract_url(&text, port)
    }

    pub fn classify(&self) -> FailureKind {
        let text = self.text();
        classify(&text)
    }
}

pub fn extract_url(text: &str, port: u16) -> Option<String> {
    let needle = format!("http://127.0.0.1:{port}");
    let mut window = text.len();
    while window >= needle.len() {
        let found = text[..window].rfind(&needle);
        let p = match found {
            None => return None,
            Some(p) => p,
        };
        let after = &text[p + needle.len()..];
        let raw: String = after
            .chars()
            .take_while(|c| !c.is_whitespace())
            .collect();
        let mut url = format!("{needle}{raw}");
        while matches!(
            url.chars().last(),
            Some(')') | Some(',') | Some(';') | Some('"') | Some('\'') | Some('。') | Some(']') | Some('}')
        ) {
            url.pop();
        }
        let tail = &url[needle.len()..];
        if tail.is_empty() || tail.starts_with('/') || tail.starts_with('?') {
            return Some(url);
        }
        window = p;
    }
    None
}

pub fn plugin_candidates(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if !line.contains("failed to import loader entry") {
            continue;
        }
        let Some(open) = line.find('(') else { continue };
        let rest = &line[open + 1..];
        let close = rest.find(')').or_else(|| rest.find(':')).unwrap_or(rest.len());
        let pkg = rest[..close].trim();
        if !pkg.is_empty() && !out.iter().any(|p| p == pkg) {
            out.push(pkg.to_string());
        }
    }
    out
}

fn looks_like_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    let drive = bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/')
        && bytes[0].is_ascii_alphabetic();
    drive || s.starts_with('/')
}

pub fn extract_lock_path(text: &str) -> Option<String> {
    if let Some(p) = text.find(LOCK_PATH_MARK) {
        let after = &text[p + LOCK_PATH_MARK.len()..];
        let tail: String = after
            .chars()
            .take_while(|c| *c != ';' && *c != '\n' && *c != '\r')
            .collect();
        let path = tail.trim().to_string();
        if looks_like_path(&path) {
            return Some(path);
        }
    }
    for token in text.split_whitespace() {
        let t = token.trim_end_matches(&[';', ',', ')', ']', '}', '"', '\'']);
        if t.ends_with(".lock") && looks_like_path(t) {
            return Some(t.to_string());
        }
    }
    None
}

pub fn classify(text: &str) -> FailureKind {
    if PLUGIN_MARKERS.iter().any(|m| text.contains(m)) {
        return FailureKind::PluginTree;
    }
    if LOCK_MARKERS.iter().any(|m| text.contains(m)) {
        return FailureKind::StaleLock {
            lock_path: extract_lock_path(text),
        };
    }
    FailureKind::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUGIN_ERR: &str = "Error: dsh: plugin tree failed to load\n  AggregateError: loader entries failed to apply\n    Error: failed to import loader entry dsh-web-ui-all (@linxin666/dsh-web-ui-all): The requested module '@deepseek-ai/dsh-settings' does not provide an export named 'settingsNamespace'\n    Error: failed to import loader entry dsh-web-ui-market (@linxin666/dsh-web-ui-all): The requested module '@deepseek-ai/dsh-settings' does not provide an export named 'installSettingsSection'";
    const LOCK_ERR: &str = "task-board ledger lock is unreadable: C:\\Users\\18129\\.dsh\\task-board\\ledger-v2.lock; if this is a leftover from an unclean shutdown and no other DSH host is running, remove it manually and retry";

    #[test]
    fn extract_url_takes_token_line() {
        let text = "dsh web: http://127.0.0.1:3080/?token=abc123";
        assert_eq!(
            extract_url(text, 3080).as_deref(),
            Some("http://127.0.0.1:3080/?token=abc123")
        );
    }

    #[test]
    fn extract_url_last_match_wins() {
        let text = "listening on http://127.0.0.1:3080\ndsh web: http://127.0.0.1:3080/?token=xyz";
        assert_eq!(
            extract_url(text, 3080).as_deref(),
            Some("http://127.0.0.1:3080/?token=xyz")
        );
    }

    #[test]
    fn extract_url_bare_line_only() {
        let text = "ready at http://127.0.0.1:3080";
        assert_eq!(extract_url(text, 3080).as_deref(), Some("http://127.0.0.1:3080"));
    }

    #[test]
    fn extract_url_ignores_other_port() {
        let text = "dsh web: http://127.0.0.1:9999/?token=abc http://127.0.0.1:3080/?token=ok";
        assert_eq!(
            extract_url(text, 3080).as_deref(),
            Some("http://127.0.0.1:3080/?token=ok")
        );
        assert_eq!(extract_url(text, 9999).as_deref(), Some("http://127.0.0.1:9999/?token=abc"));
    }

    #[test]
    fn extract_url_rejects_bad_remainder() {
        let text = "ref http://127.0.0.1:3080foo";
        assert_eq!(extract_url(text, 3080), None);
    }

    #[test]
    fn extract_url_trims_trailing_punct() {
        let text = "(visit http://127.0.0.1:3080/?token=abc), see docs";
        assert_eq!(
            extract_url(text, 3080).as_deref(),
            Some("http://127.0.0.1:3080/?token=abc")
        );
    }

    #[test]
    fn classify_plugin_tree_fixture() {
        assert_eq!(classify(PLUGIN_ERR), FailureKind::PluginTree);
    }

    #[test]
    fn plugin_candidates_dedupes_and_keeps_order() {
        let got = plugin_candidates(PLUGIN_ERR);
        assert_eq!(got, vec!["@linxin666/dsh-web-ui-all".to_string()]);
        assert!(plugin_candidates("just some log").is_empty());
    }

    #[test]
    fn classify_stale_lock_fixture() {
        let kind = classify(LOCK_ERR);
        assert!(matches!(&kind, FailureKind::StaleLock { .. }));
        if let FailureKind::StaleLock { lock_path } = kind {
            assert_eq!(
                lock_path.as_deref(),
                Some(r"C:\Users\18129\.dsh\task-board\ledger-v2.lock")
            );
        }
    }

    #[test]
    fn classify_plugin_precedes_lock() {
        let mixed = format!("{PLUGIN_ERR}\n{LOCK_ERR}");
        assert_eq!(classify(&mixed), FailureKind::PluginTree);
    }

    #[test]
    fn classify_noise_is_unknown() {
        assert_eq!(classify("hello world\nok"), FailureKind::Unknown);
        assert_eq!(classify(""), FailureKind::Unknown);
    }

    #[test]
    fn capture_joins_chunks_split_mid_cjk_char() {
        let msg = "ledger lock is unreadable: C:\\Users\\李四\\.dsh\\task-board\\ledger-v2.lock; if this is a leftover from an unclean shutdown and no other DSH host is running, remove it manually and retry";
        let bytes = msg.as_bytes();
        for cut in [1usize, bytes.len() / 3, bytes.len() / 2, bytes.len() - 3] {
            let cap2 = ServiceCapture::new(DEFAULT_LIMIT);
            cap2.push_bytes(&bytes[..cut]);
            cap2.push_bytes(&bytes[cut..]);
            let text = cap2.text();
            assert!(text.contains("李四"), "cut {cut} corrupted text: {text}");
            assert!(matches!(
                cap2.classify(),
                FailureKind::StaleLock { .. }
            ));
        }
    }

    #[test]
    fn capture_bounded_and_reset() {
        let cap = ServiceCapture::new(16 * 1024);
        let chunk = vec![b'a'; 4096];
        for _ in 0..8 {
            cap.push_bytes(&chunk);
        }
        let text = cap.text();
        assert!(text.len() <= 16 * 1024 + 4096);
        assert!(text.contains("aaaa"));
    }

    #[test]
    fn classify_works_over_capture() {
        let cap = ServiceCapture::new(DEFAULT_LIMIT);
        cap.push_bytes(LOCK_ERR.as_bytes());
        assert_eq!(cap.classify(), FailureKind::StaleLock { lock_path: Some(r"C:\Users\18129\.dsh\task-board\ledger-v2.lock".into()) });
    }
}
