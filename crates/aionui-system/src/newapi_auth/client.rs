// fork(newapi): New API HTTP client (key probe, code exchange, userinfo) — see docs/fork/newapi-overlay.md
//! HTTP client for the New API endpoints used by the managed login:
//! key probe, authorization-code exchange and userinfo.
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLIENT_ID: &str = "aionui";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    Models(Vec<String>),
    Unauthorized,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenGrant {
    pub api_key: String,
    pub access_token: String,
}

#[derive(Clone)]
pub struct NewApiClient {
    http: reqwest::Client,
}

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(15);
const USERINFO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct ModelsBody {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: Option<String>,
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct UserInfoBody {
    preferred_username: Option<String>,
}

impl NewApiClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// GET `{root}/v1/models` with the key as Bearer token.
    pub async fn probe_models(&self, root: &str, api_key: &str) -> ProbeOutcome {
        let resp = match self
            .http
            .get(format!("{root}/v1/models"))
            .bearer_auth(api_key)
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return ProbeOutcome::Unreachable,
        };
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED => return ProbeOutcome::Unauthorized,
            _ => return ProbeOutcome::Unreachable,
        }
        match resp.json::<ModelsBody>().await {
            Ok(body) => {
                let mut ids: Vec<String> = Vec::with_capacity(body.data.len());
                for m in body.data {
                    if !ids.contains(&m.id) {
                        ids.push(m.id);
                    }
                }
                ProbeOutcome::Models(ids)
            }
            Err(_) => ProbeOutcome::Unreachable,
        }
    }

    /// POST `{root}/oauth2/token` (PKCE, public client). `Err` is a message fit for display.
    pub async fn exchange_code(
        &self,
        root: &str,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenGrant, String> {
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ];
        let resp = self
            .http
            .post(format!("{root}/oauth2/token"))
            .form(&form)
            .timeout(EXCHANGE_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "newapi request timed out".to_owned()
                } else {
                    "newapi is unreachable".to_owned()
                }
            })?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|_| "newapi is unreachable".to_owned())?;
        if !status.is_success() {
            return Err(serde_json::from_slice::<ErrorBody>(&bytes)
                .ok()
                .and_then(|b| b.error_description)
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| format!("newapi returned {}", status.as_u16())));
        }
        let body: TokenBody =
            serde_json::from_slice(&bytes).map_err(|_| "newapi returned an invalid token response".to_owned())?;
        match (body.api_key, body.access_token) {
            (Some(api_key), Some(access_token)) if api_key.starts_with("sk-") => {
                Ok(TokenGrant { api_key, access_token })
            }
            _ => Err("newapi returned no api key".to_owned()),
        }
    }

    /// GET `{root}/oauth2/userinfo` and read `preferred_username`. Any failure is `None`.
    pub async fn fetch_username(&self, root: &str, access_token: &str) -> Option<String> {
        let resp = self
            .http
            .get(format!("{root}/oauth2/userinfo"))
            .bearer_auth(access_token)
            .timeout(USERINFO_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<UserInfoBody>()
            .await
            .ok()?
            .preferred_username
            .filter(|n| !n.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client() -> NewApiClient {
        NewApiClient::new(reqwest::Client::new())
    }

    async fn probe_with(resp: ResponseTemplate) -> ProbeOutcome {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(resp)
            .mount(&server)
            .await;
        client().probe_models(&server.uri(), "sk-abc").await
    }

    #[tokio::test]
    async fn probe_ok_lists_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("Authorization", "Bearer sk-abc"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data":[{"id":"gpt-4o"},{"id":"claude-sonnet-4"},{"id":"gpt-4o"}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let out = client().probe_models(&server.uri(), "sk-abc").await;
        assert_eq!(
            out,
            ProbeOutcome::Models(vec!["gpt-4o".into(), "claude-sonnet-4".into()])
        );
    }

    #[tokio::test]
    async fn probe_works_under_path_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gw/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"m"}]})))
            .expect(1)
            .mount(&server)
            .await;
        let out = client().probe_models(&format!("{}/gw", server.uri()), "sk-abc").await;
        assert_eq!(out, ProbeOutcome::Models(vec!["m".into()]));
    }

    #[tokio::test]
    async fn probe_401_is_unauthorized() {
        assert_eq!(probe_with(ResponseTemplate::new(401)).await, ProbeOutcome::Unauthorized);
    }

    #[tokio::test]
    async fn probe_403_is_unreachable() {
        assert_eq!(probe_with(ResponseTemplate::new(403)).await, ProbeOutcome::Unreachable);
    }

    #[tokio::test]
    async fn probe_500_is_unreachable() {
        assert_eq!(probe_with(ResponseTemplate::new(500)).await, ProbeOutcome::Unreachable);
    }

    #[tokio::test]
    async fn probe_html_body_is_unreachable() {
        assert_eq!(
            probe_with(ResponseTemplate::new(200).set_body_string("<html></html>")).await,
            ProbeOutcome::Unreachable
        );
    }

    #[tokio::test]
    async fn probe_missing_data_is_unreachable() {
        assert_eq!(
            probe_with(ResponseTemplate::new(200).set_body_json(json!({}))).await,
            ProbeOutcome::Unreachable
        );
    }

    #[tokio::test]
    async fn probe_connection_refused_is_unreachable() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let out = client()
            .probe_models(&format!("http://127.0.0.1:{port}"), "sk-abc")
            .await;
        assert_eq!(out, ProbeOutcome::Unreachable);
    }

    #[tokio::test]
    async fn probe_times_out_as_unreachable() {
        let start = Instant::now();
        let out = probe_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"m"}]}))
                .set_delay(Duration::from_secs(6)),
        )
        .await;
        assert_eq!(out, ProbeOutcome::Unreachable);
        assert!(start.elapsed() < Duration::from_secs(6));
    }

    #[tokio::test]
    async fn exchange_sends_pkce_form_and_returns_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token":"at","api_key":"sk-xyz","token_type":"Bearer"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let grant = client()
            .exchange_code(&server.uri(), "the-code", "the-verifier", "http://127.0.0.1:1/cb")
            .await
            .unwrap();
        assert_eq!(
            grant,
            TokenGrant {
                api_key: "sk-xyz".into(),
                access_token: "at".into()
            }
        );

        let reqs = server.received_requests().await.unwrap();
        let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(&reqs[0].body)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();
        let expected = [
            ("client_id", "aionui"),
            ("code", "the-code"),
            ("code_verifier", "the-verifier"),
            ("grant_type", "authorization_code"),
            ("redirect_uri", "http://127.0.0.1:1/cb"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        assert_eq!(pairs, expected);
        assert!(!pairs.iter().any(|(k, _)| k == "client_secret"));
    }

    #[tokio::test]
    async fn exchange_error_uses_description() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error":"invalid_grant",
                "error_description":"authorization code is invalid, expired or already used"
            })))
            .mount(&server)
            .await;
        let err = client().exchange_code(&server.uri(), "c", "v", "r").await.unwrap_err();
        assert_eq!(err, "authorization code is invalid, expired or already used");
    }

    #[tokio::test]
    async fn exchange_non_json_error_uses_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;
        let err = client().exchange_code(&server.uri(), "c", "v", "r").await.unwrap_err();
        assert_eq!(err, "newapi returned 502");
    }

    #[tokio::test]
    async fn exchange_rejects_missing_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"at"})))
            .mount(&server)
            .await;
        let err = client().exchange_code(&server.uri(), "c", "v", "r").await.unwrap_err();
        assert_eq!(err, "newapi returned no api key");
    }

    #[tokio::test]
    async fn username_reads_preferred_username() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/oauth2/userinfo"))
            .and(header("Authorization", "Bearer at"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sub":"1","preferred_username":"alice"})))
            .mount(&server)
            .await;
        assert_eq!(
            client().fetch_username(&server.uri(), "at").await.as_deref(),
            Some("alice")
        );
    }

    #[tokio::test]
    async fn username_none_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/oauth2/userinfo"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert_eq!(client().fetch_username(&server.uri(), "at").await, None);
    }
}
