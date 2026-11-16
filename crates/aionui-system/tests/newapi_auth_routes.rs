// fork(newapi): route tests for the managed New API login — see docs/fork/newapi-overlay.md
//! Black-box tests for the managed New API login routes, driven through
//! `tower::ServiceExt::oneshot` against an in-memory DB and a wiremock server.

use std::sync::Arc;

use aionui_auth::CurrentUser;
use aionui_db::{SqliteProviderRepository, UserStatus, UserType, init_database_memory};
use aionui_realtime::BroadcastEventBus;
use aionui_system::{NewApiAuthService, NewApiClient, ProviderService, newapi_auth_routes};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER: &str = "user-1";

struct Fixture {
    router: axum::Router,
    providers: ProviderService,
    server: MockServer,
    _dir: tempfile::TempDir,
    _db: aionui_db::Database,
}

async fn fixture() -> Fixture {
    let db = init_database_memory().await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, user_type, username, password_hash, status, session_generation, created_at, updated_at) \
         VALUES (?, 'local', ?, '', 'active', 0, 1, 1)",
    )
    .bind(USER)
    .bind(USER)
    .execute(db.pool())
    .await
    .unwrap();
    let providers = ProviderService::new(Arc::new(SqliteProviderRepository::new(db.pool().clone())), [0x42; 32]);
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = NewApiAuthService::new(
        providers.clone(),
        NewApiClient::new(reqwest::Client::new()),
        Arc::new(BroadcastEventBus::new(16)),
        dir.path().to_path_buf(),
    )
    .with_url_sources(Some(server.uri()), None);
    Fixture {
        router: newapi_auth_routes(svc),
        providers,
        server,
        _dir: dir,
        _db: db,
    }
}

impl Fixture {
    async fn mock_oauth_ok(&self) {
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "api_key": "sk-1", "access_token": "at" })))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oauth2/userinfo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "preferred_username": "alice" })))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [{ "id": "m1" }] })))
            .mount(&self.server)
            .await;
    }

    async fn send(&self, mut req: Request<Body>) -> axum::response::Response {
        req.extensions_mut().insert(CurrentUser {
            id: USER.to_owned(),
            username: USER.to_owned(),
            user_type: UserType::Local,
            status: UserStatus::Active,
        });
        self.router.clone().oneshot(req).await.unwrap()
    }

    async fn start(&self, host: &str) -> String {
        let resp = self.send(start_req(Some(host))).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        json["data"]["authorize_url"].as_str().unwrap().to_owned()
    }

    async fn callback(&self, query: &str) -> axum::response::Response {
        self.send(
            Request::builder()
                .uri(format!("/api/newapi/login/callback?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn status(&self) -> Value {
        let resp = self
            .send(
                Request::builder()
                    .uri("/api/newapi/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    }

    async fn stored_key(&self) -> Option<String> {
        self.providers
            .list(USER)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.id == "newapi-managed")
            .map(|p| p.api_key)
    }
}

fn start_req(host: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("POST").uri("/api/newapi/login/start");
    if let Some(h) = host {
        b = b.header(header::HOST, h);
    }
    b.body(Body::empty()).unwrap()
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    resp.into_body().collect().await.unwrap().to_bytes().to_vec()
}

async fn body_json(resp: axum::response::Response) -> Value {
    serde_json::from_slice(&body_bytes(resp).await).unwrap()
}

async fn body_text(resp: axum::response::Response) -> String {
    String::from_utf8(body_bytes(resp).await).unwrap()
}

fn query_param(url: &str, key: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .unwrap()
        .1
        .into_owned()
}

#[tokio::test]
async fn status_route_returns_needs_login_json() {
    let f = fixture().await;
    let json = f.status().await;
    assert_eq!(json["success"], true);
    assert_eq!(json["data"]["state"], "needs_login");
    assert_eq!(json["data"]["reason"], "no_token");
}

#[tokio::test]
async fn start_uses_host_header_port() {
    let f = fixture().await;
    let url = f.start("127.0.0.1:53124").await;
    assert_eq!(
        query_param(&url, "redirect_uri"),
        "http://127.0.0.1:53124/api/newapi/login/callback"
    );
    let url = f.start("localhost:53124").await;
    assert_eq!(
        query_param(&url, "redirect_uri"),
        "http://127.0.0.1:53124/api/newapi/login/callback"
    );
}

#[tokio::test]
async fn start_without_port_is_400() {
    let f = fixture().await;
    for req in [
        start_req(Some("localhost")),
        start_req(Some("localhost:abc")),
        start_req(None),
    ] {
        assert_eq!(f.send(req).await.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn full_flow_over_http() {
    let f = fixture().await;
    f.mock_oauth_ok().await;
    let url = f.start("127.0.0.1:53124").await;
    let resp = f
        .callback(&format!("code=abc&state={}", query_param(&url, "state")))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let body = body_text(resp).await;
    assert!(body.contains("Signed in"));
    assert!(body.contains("You can close this tab and return to the app."));
    assert_eq!(f.status().await["data"]["state"], "ready");
}

#[tokio::test]
async fn callback_twice_fails_second_time() {
    let f = fixture().await;
    f.mock_oauth_ok().await;
    let url = f.start("127.0.0.1:53124").await;
    let q = format!("code=abc&state={}", query_param(&url, "state"));
    assert_eq!(f.callback(&q).await.status(), StatusCode::OK);
    let key = f.stored_key().await;
    assert!(key.is_some());
    let resp = f.callback(&q).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
    assert!(body_text(resp).await.contains("Sign-in failed"));
    assert_eq!(f.stored_key().await, key);
}

#[tokio::test]
async fn callback_after_second_start_fails_for_first_state() {
    let f = fixture().await;
    f.mock_oauth_ok().await;
    let first = f.start("127.0.0.1:53124").await;
    let second = f.start("127.0.0.1:53124").await;
    let resp = f
        .callback(&format!("code=abc&state={}", query_param(&first, "state")))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_text(resp).await.contains("Sign-in failed"));
    assert!(f.stored_key().await.is_none());
    let resp = f
        .callback(&format!("code=abc&state={}", query_param(&second, "state")))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_text(resp).await.contains("Signed in"));
}

#[tokio::test]
async fn callback_escapes_error_message() {
    let f = fixture().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "invalid_grant", "error_description": "<script>alert(1)</script>" })),
        )
        .mount(&f.server)
        .await;
    let url = f.start("127.0.0.1:53124").await;
    let resp = f
        .callback(&format!("code=abc&state={}", query_param(&url, "state")))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_text(resp).await;
    assert!(body.contains("Sign-in failed"));
    assert!(!body.contains("<script>"));
    assert!(body.contains("&lt;script&gt;"));
}

#[tokio::test]
async fn callback_without_params_shows_failure_page() {
    let f = fixture().await;
    let resp = f.callback("").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_text(resp).await.contains("Sign-in failed"));
}

fn req_with(method_: &str, uri: &str, host: Option<&str>, origin: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method_).uri(uri);
    if let Some(h) = host {
        b = b.header(header::HOST, h);
    }
    if let Some(o) = origin {
        b = b.header(header::ORIGIN, o);
    }
    b.body(Body::empty()).unwrap()
}

const START: &str = "/api/newapi/login/start";
const STATUS: &str = "/api/newapi/status";
const CALLBACK: &str = "/api/newapi/login/callback";

#[tokio::test]
async fn start_with_foreign_origin_is_403_and_creates_no_pending_login() {
    let f = fixture().await;
    f.mock_oauth_ok().await;
    let resp = f
        .send(req_with(
            "POST",
            START,
            Some("127.0.0.1:53124"),
            Some("https://evil.example"),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_text(f.callback("code=abc&state=anything").await).await;
    assert!(body.contains("Sign-in failed"));
    assert!(f.stored_key().await.is_none());
}

#[tokio::test]
async fn start_allows_loopback_null_and_missing_origin() {
    let f = fixture().await;
    for origin in [
        Some("http://localhost:5173"),
        Some("null"),
        Some("file://"),
        Some("app://aionui"),
        None,
    ] {
        let resp = f.send(req_with("POST", START, Some("127.0.0.1:53124"), origin)).await;
        assert_eq!(resp.status(), StatusCode::OK, "{origin:?}");
    }
}

#[tokio::test]
async fn status_with_foreign_origin_is_403() {
    let f = fixture().await;
    let resp = f
        .send(req_with(
            "GET",
            STATUS,
            Some("127.0.0.1:13400"),
            Some("http://evil.example"),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = f
        .send(req_with(
            "GET",
            STATUS,
            Some("127.0.0.1:13400"),
            Some("http://localhost:5173"),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn foreign_host_is_403_on_all_routes() {
    let f = fixture().await;
    for (m, uri) in [("POST", START), ("GET", STATUS), ("GET", CALLBACK)] {
        let resp = f.send(req_with(m, uri, Some("evil.example:13400"), None)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }
}

#[tokio::test]
async fn loopback_host_spellings_are_accepted() {
    let f = fixture().await;
    for host in ["LOCALHOST:13400", "[::1]:13400", "127.0.0.1:13400"] {
        for (m, uri) in [("POST", START), ("GET", STATUS)] {
            let resp = f.send(req_with(m, uri, Some(host), None)).await;
            assert_eq!(resp.status(), StatusCode::OK, "{host} {uri}");
        }
        let resp = f.send(req_with("GET", CALLBACK, Some(host), None)).await;
        assert_eq!(resp.status(), StatusCode::OK, "{host} callback");
    }
}
