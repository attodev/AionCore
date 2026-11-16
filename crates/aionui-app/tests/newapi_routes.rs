// fork(newapi): app-level tests for the newapi routes — see docs/fork/newapi-overlay.md
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

async fn router(local: bool) -> (axum::Router, aionui_app::AppServices) {
    let db = aionui_db::init_database_memory().await.unwrap();
    let config = aionui_app::AppConfig {
        local,
        ..Default::default()
    };
    let services = aionui_app::AppServices::from_config(db, &config).await.unwrap();
    let router = aionui_app::create_router(&services).await.expect("build router");
    (router, services)
}

#[tokio::test]
async fn newapi_routes_absent_when_not_local() {
    let (router, services) = router(false).await;
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/newapi/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Not registered: the auth layer never sees it, so it is a plain 404
    // (a registered-but-protected route would be 401 in non-local mode).
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    services.database.close().await;
}

#[tokio::test]
async fn newapi_status_present_in_local_mode() {
    let (router, services) = router(true).await;
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/newapi/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["success"], true);
    services.database.close().await;
}

#[tokio::test]
async fn newapi_callback_failure_page_survives_app_middleware() {
    let (router, services) = router(true).await;
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/newapi/login/callback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers()[header::CONTENT_TYPE].to_str().unwrap().to_owned();
    assert!(ct.starts_with("text/html"), "content-type was {ct}");
    let body = String::from_utf8(to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
    assert!(body.contains("Sign-in failed"), "{body}");
    services.database.close().await;
}
