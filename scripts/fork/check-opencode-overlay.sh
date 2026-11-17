#!/usr/bin/env bash
# Verify the opencode overlay: fork-owned files exist and upstream hooks are present.
# Marker checks are exact-string matches: they fail loudly by design if a
# formatter rewraps a hook line (then update the hook, doc and this script).
set -euo pipefail
cd "$(dirname "$0")/../.."

fail=0
hooks=0
negs=0
LOCK_VERSION=2.3.0   # must match docs/fork/opencode-overlay.md "Pinned version"
files=(
  crates/aionui-runtime/resources/opencode-lock.json
  crates/aionui-runtime/src/opencode_fork/mod.rs
  crates/aionui-runtime/src/opencode_fork/bundle.rs
  crates/aionui-app/src/commands/opencode_bundle_fork.rs
  crates/aionui-ai-agent/src/factory/opencode_fork.rs
  docs/fork/opencode-probe-notes.md
  docs/fork/opencode-overlay.md
)
for f in "${files[@]}"; do
  [ -f "$f" ] || { echo "MISSING fork-owned file: $f" >&2; fail=1; }
done
# Source files must carry the fork header.
for f in "${files[@]}"; do
  case "$f" in
    *.rs) head -n1 "$f" 2>/dev/null | grep -qF '// fork(opencode):' \
      || { echo "MISSING fork header (line 1) in $f" >&2; fail=1; } ;;
  esac
done

hook() { # file, fixed pattern, description
  hooks=$((hooks + 1))
  grep -qF -- "$2" "$1" 2>/dev/null || { echo "MISSING hook: $3 in $1" >&2; fail=1; }
}

R=crates/aionui-runtime/src
hook $R/resolver.rs 'crate::opencode_fork::return_if_overridden!(cmd); // fork(opencode)' 'resolver override macro'
hook $R/lib.rs 'pub mod opencode_fork; // fork(opencode)' 'opencode_fork module declaration (runtime)'
hook crates/aionui-runtime/Cargo.toml 'wiremock.workspace = true # fork(opencode)' 'wiremock dev-dependency (bundle download tests)'
A=crates/aionui-app/src/commands
hook $A/mod.rs 'pub(crate) mod opencode_bundle_fork; // fork(opencode)' 'opencode_bundle_fork module declaration'
hook $A/cmd_prepare_managed_resources.rs 'let opencode_clis = super::opencode_bundle_fork::bundle_for_contract(&output_root).await?; // fork(opencode)' 'bundling call'
hook $A/cmd_prepare_managed_resources.rs 'clis: opencode_clis, // fork(opencode)' 'contract clis field'
F=crates/aionui-ai-agent/src/factory
hook $F/mod.rs 'mod opencode_fork; // fork(opencode)' 'opencode_fork module declaration (factory)'
hook $F/acp.rs 'super::opencode_fork::apply_opencode_env(&deps, &ctx, &config, &mut command_spec).await; // fork(opencode)' 'env overlay call'

# The resolver hook must be the first statement of resolve_command_path.
negs=$((negs + 1))
first=$(grep -A1 'pub fn resolve_command_path' $R/resolver.rs | tail -n1)
case "$first" in
  *return_if_overridden*) ;;
  *) echo "ORDER: return_if_overridden! must be the first statement of resolve_command_path" >&2; fail=1 ;;
esac
# The acp.rs hook must come after the launch policy call.
negs=$((negs + 1))
pol=$(grep -n 'apply_acp_launch_policy(' $F/acp.rs | head -n1 | cut -d: -f1 || true)
hk=$(grep -n 'super::opencode_fork::apply_opencode_env' $F/acp.rs | head -n1 | cut -d: -f1 || true)
{ [ -n "$pol" ] && [ -n "$hk" ] && [ "$hk" -gt "$pol" ]; } \
  || { echo "ORDER: apply_opencode_env must come after apply_acp_launch_policy in $F/acp.rs" >&2; fail=1; }

# Negative checks: leftovers from a bad conflict resolution.
negs=$((negs + 1))
c=$(grep -c opencode $R/resolver.rs || true)
[ "$c" -eq 1 ] || { echo "LEFTOVER: $R/resolver.rs must have exactly one opencode line, found $c" >&2; fail=1; }
negs=$((negs + 1))
c=$(grep -c opencode_fork $F/acp.rs || true)
[ "$c" -eq 1 ] || { echo "LEFTOVER: $F/acp.rs must have exactly one opencode_fork line, found $c" >&2; fail=1; }

# Marker lines in pre-existing files must equal the hooks checked above.
negs=$((negs + 1))
total=$(grep -rF 'fork(opencode)' crates --include=*.rs --include=Cargo.toml | wc -l)
owned=0
for f in "${files[@]}"; do
  [ -f "$f" ] && [ "${f#crates/}" != "$f" ] && owned=$((owned + $(grep -cF 'fork(opencode)' "$f" || true)))
done
upstream=$((total - owned))
[ "$upstream" -eq "$hooks" ] || { echo "LEFTOVER: $upstream fork(opencode) marker lines in pre-existing files, expected $hooks" >&2; fail=1; }

# Files the newapi overlay keeps pristine must not be touched by this overlay.
negs=$((negs + 1))
if grep -q opencode crates/aionui-app/src/router/state.rs; then
  echo "LEFTOVER: router/state.rs must not mention opencode (it must stay identical to upstream)" >&2; fail=1
fi

# Lock file: valid JSON and version equals the documented one.
negs=$((negs + 1))
lock=crates/aionui-runtime/resources/opencode-lock.json
v=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$lock" 2>/dev/null || true)
[ "$v" = "$LOCK_VERSION" ] || { echo "LOCK: $lock version '$v' is not valid JSON or differs from $LOCK_VERSION" >&2; fail=1; }
grep -qF "\`$LOCK_VERSION\`" docs/fork/opencode-overlay.md \
  || { echo "DOC: docs/fork/opencode-overlay.md does not state version $LOCK_VERSION" >&2; fail=1; }

if [ "$fail" -ne 0 ]; then
  echo "opencode overlay check FAILED" >&2
  exit 1
fi
echo "opencode overlay OK: ${#files[@]} fork-owned files, $hooks hook checks, $negs negative checks"
