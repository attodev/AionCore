// fork(newapi): HTTP routes for the managed New API login — see docs/fork/newapi-overlay.md
#![allow(clippy::disallowed_types)]

//! HTTP routes for the managed New API login.
//!
//! - `GET  /api/newapi/status`
//! - `POST /api/newapi/login/start` (no request body; port comes from `Host`)
//! - `GET  /api/newapi/login/callback` (browser navigation, answers HTML)

use aionui_api_types::{ApiResponse, NewApiLoginStartResponse, NewApiStatusResponse};
use aionui_auth::CurrentUser;
use aionui_common::ApiError;
use axum::Router;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use super::service::NewApiAuthService;

/// Build the New API login router. Authentication is applied by the caller.
pub fn newapi_auth_routes(service: NewApiAuthService) -> Router {
    Router::new()
        .route("/api/newapi/status", get(status))
        .route("/api/newapi/login/start", post(start_login))
        .route("/api/newapi/login/callback", get(callback))
        .with_state(service)
}

const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Host name of a `Host` header value (port stripped, lowercased, IPv6 keeps its brackets).
fn host_name(value: &str) -> String {
    let name = match value.strip_prefix('[') {
        Some(rest) => rest
            .split_once(']')
            .map_or(value, |(inner, _)| &value[..inner.len() + 2]),
        None => value.rsplit_once(':').map_or(value, |(h, _)| h),
    };
    name.to_ascii_lowercase()
}

/// 403 unless the `Host` header (when present) names a loopback address. Blocks DNS rebinding.
fn check_host(headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok())
        && !LOOPBACK_HOSTS.contains(&host_name(host).as_str())
    {
        return Err(ApiError::Forbidden("Host not allowed".into()));
    }
    Ok(())
}

/// 403 when a web page from a non-loopback http(s) origin is calling. Opaque (`null`), `file:`
/// and custom-scheme origins (the desktop renderer) pass. Blocks cross-site login CSRF.
fn check_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let Some(url) = origin.to_str().ok().and_then(|o| url::Url::parse(o).ok()) else {
        return Ok(());
    };
    if matches!(url.scheme(), "http" | "https")
        && !url
            .host_str()
            .is_some_and(|h| LOOPBACK_HOSTS.contains(&h.to_ascii_lowercase().as_str()))
    {
        return Err(ApiError::Forbidden("Origin not allowed".into()));
    }
    Ok(())
}

async fn status(
    State(service): State<NewApiAuthService>,
    Extension(user): Extension<CurrentUser>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<NewApiStatusResponse>>, ApiError> {
    check_host(&headers)?;
    check_origin(&headers)?;
    Ok(Json(ApiResponse::ok(service.status(&user.id).await?)))
}

async fn start_login(
    State(service): State<NewApiAuthService>,
    Extension(_user): Extension<CurrentUser>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<NewApiLoginStartResponse>>, ApiError> {
    check_host(&headers)?;
    check_origin(&headers)?;
    let port = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|host| host.rsplit_once(':'))
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .ok_or_else(|| ApiError::BadRequest("Host header must include a port".into()))?;
    Ok(Json(ApiResponse::ok(service.start_login(port)?)))
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(service): State<NewApiAuthService>,
    Extension(user): Extension<CurrentUser>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    // Top-level browser navigation: Host check only (there is no Origin to judge).
    if let Err(e) = check_host(&headers) {
        return e.into_response();
    }
    let result = service
        .complete_login(&user.id, q.code.as_deref(), q.state.as_deref(), q.error.as_deref())
        .await;
    // Failure is also 200: only a browser sees this page, and the app's boundary
    // middleware would replace a non-JSON 400 with a JSON error body.
    match result {
        Ok(()) => html_page(
            StatusCode::OK,
            "Signed in",
            "You can close this tab and return to the app.",
        ),
        Err(message) => html_page(StatusCode::OK, "Sign-in failed", &message),
    }
}

fn html_page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title>\
         <style>body{{font-family:system-ui,sans-serif;display:flex;min-height:100vh;margin:0;\
         align-items:center;justify-content:center;text-align:center}}main{{padding:24px}}</style>\
         </head><body><main><h1>{title}</h1><p>{body}</p></main></body></html>",
        title = escape_html(title),
        body = escape_html(body),
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        html,
    )
        .into_response()
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}
