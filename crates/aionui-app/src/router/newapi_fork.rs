// fork(newapi): managed New API login wiring for the app router — see docs/fork/newapi-overlay.md
//! Fork-owned glue that mounts the managed New API login routes.

use std::sync::Arc;

use axum::Router;
use axum::middleware::from_fn_with_state;

use aionui_auth::{AuthState, auth_middleware};
use aionui_db::SqliteProviderRepository;
use aionui_system::ProviderService;

use crate::config::derive_encryption_key;
use crate::services::AppServices;

/// Build the managed New API login service.
fn build_newapi_auth_service(services: &AppServices) -> aionui_system::NewApiAuthService {
    let encryption_key = derive_encryption_key(&services.encryption_secret_raw);
    let provider_repo = Arc::new(SqliteProviderRepository::new(services.database.pool().clone()));
    aionui_system::NewApiAuthService::new(
        ProviderService::new(provider_repo, encryption_key),
        aionui_system::NewApiClient::new(reqwest::Client::new()),
        services.event_bus.clone(),
        services.data_dir.clone(),
    )
}

/// Merge the managed New API login routes: local mode only (the default user is
/// injected by the auth middleware, so the browser's callback navigation needs
/// no credentials).
pub(super) fn merge_newapi_routes(router: Router, services: &AppServices, auth_mw_state: AuthState) -> Router {
    if services.identity_mode.is_local() {
        router.merge(
            aionui_system::newapi_auth_routes(build_newapi_auth_service(services))
                .route_layer(from_fn_with_state(auth_mw_state, auth_middleware)),
        )
    } else {
        router
    }
}
