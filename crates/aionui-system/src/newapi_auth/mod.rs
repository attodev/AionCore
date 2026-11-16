// fork(newapi): New API login module root — see docs/fork/newapi-overlay.md
//! Managed New API login: client configuration and base-URL resolution.
pub mod client;
pub mod config;
pub mod pkce;
pub mod routes;
pub mod service;

pub use client::NewApiClient;
pub use routes::newapi_auth_routes;
pub use service::NewApiAuthService;
