#!/usr/bin/env bash
# Verify the newapi overlay: fork-owned files exist and upstream hooks are present.
# Marker checks are exact-string matches: they fail loudly by design if a
# formatter rewraps a hook line (then update the hook, doc and this script).
set -euo pipefail
cd "$(dirname "$0")/../.."

fail=0
hooks=0
files=(
  crates/aionui-system/src/newapi_auth/mod.rs
  crates/aionui-system/src/newapi_auth/config.rs
  crates/aionui-system/src/newapi_auth/client.rs
  crates/aionui-system/src/newapi_auth/pkce.rs
  crates/aionui-system/src/newapi_auth/routes.rs
  crates/aionui-system/src/newapi_auth/service.rs
  crates/aionui-api-types/src/newapi_auth.rs
  crates/aionui-app/src/router/newapi_fork.rs
  crates/aionui-system/tests/newapi_auth_routes.rs
  crates/aionui-app/tests/newapi_routes.rs
  docs/fork/newapi-overlay.md
)
for f in "${files[@]}"; do
  [ -f "$f" ] || { echo "MISSING fork-owned file: $f" >&2; fail=1; }
done

hook() { # file, fixed pattern, description
  hooks=$((hooks + 1))
  grep -qF -- "$2" "$1" 2>/dev/null || { echo "MISSING hook: $3 in $1" >&2; fail=1; }
}
hook_re() { # file, regex, description
  hooks=$((hooks + 1))
  grep -qE -- "$2" "$1" 2>/dev/null || { echo "MISSING hook: $3 in $1" >&2; fail=1; }
}

R=crates/aionui-app/src/router
hook $R/routes.rs 'merge_newapi_routes(router, services, auth_mw_state.clone()); // fork(newapi)' 'newapi route merge'
hook $R/routes.rs ') || super::newapi_fork::is_global_event(event_name) // fork(newapi)' 'newapi global event'
hook $R/mod.rs 'mod newapi_fork; // fork(newapi)' 'newapi_fork module declaration'
hook crates/aionui-system/src/lib.rs 'pub mod newapi_auth; // fork(newapi)' 'newapi_auth module declaration'
hook crates/aionui-system/src/lib.rs 'newapi_auth_routes}; // fork(newapi)' 'newapi_auth re-exports'
hook crates/aionui-api-types/src/lib.rs 'mod newapi_auth; // fork(newapi)' 'newapi_auth module declaration'
hook_re crates/aionui-api-types/src/lib.rs '^pub use newapi_auth::\{' 'newapi_auth re-export'
C=crates/aionui-system/Cargo.toml
for d in url sha2 base64 getrandom tempfile; do
  hook_re $C "^${d}\\.workspace" "dependency '${d}'"
done
hooks=$((hooks + 1))
n=$(grep -cF '# fork(newapi)' $C || true)
[ "$n" -ge 2 ] || { echo "MISSING hook: need 2 '# fork(newapi)' markers ([dependencies], [dev-dependencies]) in $C" >&2; fail=1; }

# Negative checks: leftovers from a bad conflict resolution.
if grep -q newapi $R/state.rs; then
  echo "LEFTOVER: $R/state.rs must not mention newapi (it must stay identical to upstream)" >&2; fail=1
fi
c=$(grep -c newapi $R/routes.rs || true)
[ "$c" -eq 3 ] || { echo "LEFTOVER: $R/routes.rs must have exactly three newapi lines (route merge, global event, its test), found $c" >&2; fail=1; }

if [ "$fail" -ne 0 ]; then
  echo "newapi overlay check FAILED" >&2
  exit 1
fi
echo "newapi overlay OK: ${#files[@]} fork-owned files, $hooks hook checks, 2 negative checks"
