# newapi login overlay (AionCore)

Overlay base: 503a8719 (update this after each upstream sync)

Desktop sign-in to a newapi gateway. AionCore runs the PKCE/OIDC authorization-code flow against the gateway's native OIDC client, stores the resulting key as a managed provider, and emits a realtime event when the login state changes. It pairs with the AionUi overlay (the desktop UI that calls these routes) and with the gateway's native OIDC client. Everything is additive and local-mode only; all upstream edits are single marked hook lines (`fork(newapi)`).

## Fork-owned files

| Path | Responsibility |
| --- | --- |
| `crates/aionui-system/src/newapi_auth/mod.rs` | Module root |
| `crates/aionui-system/src/newapi_auth/config.rs` | Client config and base-URL resolution |
| `crates/aionui-system/src/newapi_auth/client.rs` | HTTP client: key probe, code exchange, userinfo |
| `crates/aionui-system/src/newapi_auth/pkce.rs` | PKCE helpers, authorize URL |
| `crates/aionui-system/src/newapi_auth/routes.rs` | The three HTTP routes |
| `crates/aionui-system/src/newapi_auth/service.rs` | Status decision, pending login, managed-provider upsert, `newapi.authChanged` event |
| `crates/aionui-api-types/src/newapi_auth.rs` | Wire DTOs and event payload |
| `crates/aionui-app/src/router/newapi_fork.rs` | Service construction and local-mode-only route merge |
| `crates/aionui-system/tests/newapi_auth_routes.rs` | Route tests |
| `crates/aionui-app/tests/newapi_routes.rs` | App-level route tests |
| `scripts/fork/check-newapi-overlay.sh` | Verifies files and hooks are present |
| `docs/fork/newapi-overlay.md` | This document |

## Hooks in upstream files

Every hook carries the marker `fork(newapi)`. Find them all with `grep -rn "fork(newapi)" crates --include=*.rs --include=Cargo.toml`.

| File | Line(s) | Why | Re-apply if code moved |
| --- | --- | --- | --- |
| `crates/aionui-app/src/router/routes.rs` | `let router = super::newapi_fork::merge_newapi_routes(router, services, auth_mw_state.clone()); // fork(newapi)` | Mount routes | Insert immediately before the `let router = if services.identity_mode.is_local() { router } else { router.layer(csrf...) }` statement (right after the `#[cfg(feature = "weixin")]` merge), so the routes stay under the security-headers, body-limit, boundary-error and access-log layers. Do not place it after any `.layer(...)` call. |
| `crates/aionui-app/src/router/mod.rs` | `mod newapi_fork; // fork(newapi)` | Declare module | Add to the `mod` list |
| `crates/aionui-system/src/lib.rs` | `pub mod newapi_auth; // fork(newapi)` and `pub use newapi_auth::{NewApiAuthService, NewApiClient, newapi_auth_routes}; // fork(newapi)` | Expose module and re-exports | Add next to the other `pub mod` / `pub use` lines |
| `crates/aionui-api-types/src/lib.rs` | `mod newapi_auth; // fork(newapi)` and the `// fork(newapi)` comment above `pub use newapi_auth::{NewApiAuthChangedEvent, NewApiAuthReason, NewApiAuthState, NewApiLoginStartResponse, NewApiStatusResponse};` | Expose DTOs | Add alongside the other module declarations / re-exports |
| `crates/aionui-system/Cargo.toml` | Under `# fork(newapi)` in `[dependencies]`: `url`, `sha2`, `base64`, `getrandom` (all `.workspace = true`); under `# fork(newapi)` in `[dev-dependencies]`: `tempfile.workspace = true` | Dependencies of the module | Re-add at the end of each section; if a crate is missing from the workspace table, add it to the root `Cargo.toml` too |

## Mechanical / generated changes

| File | Handling |
| --- | --- |
| `Cargo.lock` | Additions: `base64`, `getrandom`, `sha2`, `tempfile` and `url` entries in the `aionui-system` dependency list. Regenerate with a cargo build (`cargo build -p aionui-system`); never hand-merge. On conflict take upstream's version and rebuild. |

## Runtime contract

| Item | Value |
| --- | --- |
| HTTP routes | Three routes registered by `newapi_auth_routes` (see `newapi_auth/routes.rs`): status, login start, login callback. Local mode only, behind the auth middleware. |
| Realtime event | `newapi.authChanged` |
| Managed provider id | `newapi-managed` |
| Client config file | `<data_dir>/newapi-client.json` |
| Runtime env | `AIONUI_NEWAPI_BASE_URL` (gateway base URL) |
| Build-time env | `AIONUI_NEWAPI_DEFAULT_URL` (default gateway URL baked in) |

## Syncing with upstream

The upstream remote is `upstream` (iOfficeAI/AionCore); `origin` is the fork (attodev/AionCore).

1. `git fetch upstream`
2. `git rebase --onto upstream/main <old-base> feat/newapi-login` (`<old-base>` is the "Overlay base" commit above).
3. Expected conflicts: only the hook lines above (and `Cargo.lock`). Resolve by re-adding the one-liners from the hooks table; for `Cargo.lock` take upstream's and rebuild.
4. Run `bash scripts/fork/check-newapi-overlay.sh`.
5. `cargo test -p aionui-system`, `cargo test -p aionui-app --test newapi_routes`.
6. `cargo clippy -p aionui-system -p aionui-app --all-targets -- -D warnings` and `cargo fmt --all -- --check`.
7. Update "Overlay base" at the top of this file to the new upstream commit.

Note: AionUi's `package.json` `aioncoreVersion` pins the upstream AionCore release that the desktop app downloads. A patched AionCore must be supplied via the `AIONUI_BACKEND_BIN` env var or a fork release.

## Commit layout

The branch is kept as three commits to make rebases easy: (1) fork-owned files, (2) hooks and mechanical edits in upstream files (including `Cargo.toml` and `Cargo.lock`), (3) docs and check script. Conflicts during a rebase should only ever occur in commit (2).

## Known follow-ups

- `Origin: null` login-CSRF residual: closing it needs a per-launch secret.
- Log the outcomes of early-return callback paths.
