// fork(newapi): PKCE helpers and authorize URL — see docs/fork/newapi-overlay.md
//! PKCE helpers and the New API authorize URL for the managed login.
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::client::CLIENT_ID;

pub const CALLBACK_PATH: &str = "/api/newapi/login/callback";
const DEVICE_NAME_FALLBACK: &str = "desktop";
const DEVICE_NAME_MAX_CHARS: usize = 64;

/// 32 random bytes as base64url without padding (43 chars).
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS random source unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256 challenge: base64url(SHA256(verifier)).
pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}{CALLBACK_PATH}")
}

/// Builds the authorize URL. `root` is a normalized root URL without a trailing slash;
/// an unparseable root is an error rather than a panic.
pub fn authorize_url(
    root: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    device_name: &str,
) -> Result<String, url::ParseError> {
    let mut url = url::Url::parse(&format!("{root}/oauth2/authorize"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", "openid")
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("device_name", device_name);
    Ok(url.into())
}

/// Host name for labeling this device: `COMPUTERNAME`, `HOSTNAME`, `/etc/hostname`, else "desktop".
pub fn device_name() -> String {
    let name = ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .chain(std::fs::read_to_string("/etc/hostname").ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| DEVICE_NAME_FALLBACK.to_string());
    name.chars().take(DEVICE_NAME_MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn challenge_matches_rfc7636_vector() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_token_is_43_urlsafe_chars_and_unique() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 43);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(a, b);
    }

    #[test]
    fn redirect_uri_uses_loopback_and_port() {
        assert_eq!(redirect_uri(53124), "http://127.0.0.1:53124/api/newapi/login/callback");
    }

    fn query_of(url: &url::Url) -> HashMap<String, String> {
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[test]
    fn authorize_url_carries_all_parameters() {
        let ru = redirect_uri(53124);
        let raw = authorize_url("https://a.example", &ru, "st&te", "chal", "내 노트북 1").unwrap();
        let url = url::Url::parse(&raw).unwrap();
        assert_eq!(url.path(), "/oauth2/authorize");
        let q = query_of(&url);
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "aionui");
        assert_eq!(q["scope"], "openid");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st&te");
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["redirect_uri"], ru);
        assert_eq!(q["device_name"], "내 노트북 1");
    }

    #[test]
    fn authorize_url_keeps_root_path_prefix() {
        let raw = authorize_url("https://a.example/gw", "http://127.0.0.1:1/x", "s", "c", "d").unwrap();
        assert_eq!(url::Url::parse(&raw).unwrap().path(), "/gw/oauth2/authorize");
    }

    #[test]
    fn device_name_is_non_empty_and_at_most_64_chars() {
        let n = device_name();
        assert!(!n.is_empty());
        assert!(n.chars().count() <= 64);
    }

    #[test]
    fn authorize_url_rejects_unparseable_root() {
        assert!(authorize_url("https://exa mple.com", "http://127.0.0.1:1/x", "s", "c", "d").is_err());
    }
}
