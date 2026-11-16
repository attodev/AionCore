// fork(newapi): New API login client configuration and base-URL resolution — see docs/fork/newapi-overlay.md
use std::path::Path;

use serde::Deserialize;

/// Client config file name, stored in the data directory.
pub const CONFIG_FILE_NAME: &str = "newapi-client.json";
/// Environment variable that supplies the New API root address.
pub const ENV_BASE_URL: &str = "AIONUI_NEWAPI_BASE_URL";
/// Root address baked in at build time, if any.
pub const BUILT_IN_DEFAULT_URL: Option<&str> = option_env!("AIONUI_NEWAPI_DEFAULT_URL");

#[derive(Deserialize)]
struct ClientConfigFile {
    base_url: Option<String>,
}

/// Trim, strip trailing `/` and a trailing `/v1`. Only plain http/https addresses with a host
/// are accepted: whitespace, backslashes, query, fragment and userinfo are rejected.
pub fn normalize_root(raw: &str) -> Option<String> {
    let mut s = raw.trim().trim_end_matches('/');
    if let Some(rest) = s.strip_suffix("/v1") {
        s = rest.trim_end_matches('/');
    }
    if s.chars().any(|c| c.is_whitespace() || matches!(c, '\\' | '?' | '#')) {
        return None;
    }
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
    if rest.is_empty() {
        return None;
    }
    let url = url::Url::parse(s).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(s.to_owned())
}

fn from_file(data_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(data_dir.join(CONFIG_FILE_NAME)).ok()?;
    let cfg: ClientConfigFile = serde_json::from_str(&text).ok()?;
    normalize_root(&cfg.base_url?)
}

/// Resolve the root address: config file > env > built-in default.
/// The file is read on every call so edits apply without a restart.
pub fn resolve_base_url(data_dir: &Path, env: Option<&str>, built_in: Option<&str>) -> Option<String> {
    from_file(data_dir)
        .or_else(|| env.and_then(normalize_root))
        .or_else(|| built_in.and_then(normalize_root))
}

/// Provider `base_url` for a root address.
pub fn provider_base_url(root: &str) -> String {
    format!("{root}/v1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_root_cases() {
        assert_eq!(
            normalize_root("https://a.example/").as_deref(),
            Some("https://a.example")
        );
        assert_eq!(
            normalize_root(" https://a.example/v1/ ").as_deref(),
            Some("https://a.example")
        );
        assert_eq!(
            normalize_root("http://10.0.0.5:3000/v1").as_deref(),
            Some("http://10.0.0.5:3000")
        );
        assert_eq!(
            normalize_root("https://a.example/gw/v1").as_deref(),
            Some("https://a.example/gw")
        );
        for bad in ["", "   ", "ftp://a", "a.example", "https://", "https:///v1"] {
            assert_eq!(normalize_root(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn normalize_root_rejects_unparseable_or_decorated_addresses() {
        for bad in [
            "https://exa mple.com",
            "https://a.example?x=1",
            "https://a.example/?x=1",
            "https://a.example#f",
            "https://a.example/gw#f",
            "https://user@a.example",
            "https://user:pw@a.example",
            "https://a.example/g w",
            "https://a.example\\gw",
            "https://:8080",
            "http://[::1",
        ] {
            assert_eq!(normalize_root(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bad_file_address_falls_through_to_env() {
        let dir = dir_with(Some(r#"{"base_url":"https://exa mple.com"}"#));
        let got = resolve_base_url(dir.path(), Some("https://e.example"), None);
        assert_eq!(got.as_deref(), Some("https://e.example"));
        let dir = dir_with(Some(r#"{"base_url":"https://a.example?x=1"}"#));
        assert_eq!(resolve_base_url(dir.path(), None, None), None);
    }

    fn dir_with(content: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(c) = content {
            std::fs::write(dir.path().join(CONFIG_FILE_NAME), c).unwrap();
        }
        dir
    }

    #[test]
    fn file_wins_over_env_and_built_in() {
        let dir = dir_with(Some(r#"{"base_url":"https://f.example/v1"}"#));
        let got = resolve_base_url(dir.path(), Some("https://e.example"), Some("https://b.example"));
        assert_eq!(got.as_deref(), Some("https://f.example"));
    }

    #[test]
    fn falls_back_env_then_built_in_then_none() {
        let dir = dir_with(None);
        let p = dir.path();
        assert_eq!(
            resolve_base_url(p, Some("https://e.example/"), Some("https://b.example")).as_deref(),
            Some("https://e.example")
        );
        assert_eq!(
            resolve_base_url(p, None, Some("https://b.example")).as_deref(),
            Some("https://b.example")
        );
        assert_eq!(
            resolve_base_url(p, Some(""), Some("https://b.example")).as_deref(),
            Some("https://b.example")
        );
        assert_eq!(resolve_base_url(p, None, None), None);
    }

    #[test]
    fn bad_file_falls_through_to_env() {
        for content in ["{not json", r#"{"other":1}"#, r#"{"base_url":""}"#] {
            let dir = dir_with(Some(content));
            let got = resolve_base_url(dir.path(), Some("https://e.example"), None);
            assert_eq!(got.as_deref(), Some("https://e.example"), "{content}");
        }
    }

    #[test]
    fn provider_base_url_appends_v1() {
        assert_eq!(provider_base_url("https://a.example"), "https://a.example/v1");
    }
}
