# opencode bundling overlay (AionCore)

Overlay base: 503a8719 (update this after each upstream sync)

Requires the newapi overlay, branch `feat/newapi-login`, applied first (see `docs/fork/newapi-overlay.md`). This overlay is branch `feat/opencode-code-tab`, stacked on top of it.

Bundles a pinned opencode (version `2.3.0`) with AionCore and launches it as an ACP agent that can only talk to the newapi gateway. The executable is resolved from an env override, then the managed-resources bundle, then PATH. When the `opencode` backend is spawned, AionCore always injects an environment overlay: with a usable `newapi-managed` provider it carries a newapi-only provider config and the key; without one it carries a deny-all config and an empty `AIONUI_NEWAPI_KEY` (fail closed). Logic lives in new files; every edit to an upstream file is a single marked line (`fork(opencode)`).

## Fork-owned files

Each source file starts with a `// fork(opencode): ... — see docs/fork/opencode-overlay.md` header.

| Path | Responsibility |
| --- | --- |
| `crates/aionui-runtime/resources/opencode-lock.json` | Version lock: `version`, and per runtime key the asset file name, `sha256` and executable name |
| `crates/aionui-runtime/src/opencode_fork/mod.rs` | Lock parsing, executable resolution (env, bundle, PATH), the `return_if_overridden!` macro |
| `crates/aionui-runtime/src/opencode_fork/bundle.rs` | Download of the locked asset, SHA-256 check, tar.gz / zip extraction, placement in the bundle |
| `crates/aionui-app/src/commands/opencode_bundle_fork.rs` | Glue that runs the bundling step inside `prepare-managed-resources` |
| `crates/aionui-ai-agent/src/factory/opencode_fork.rs` | Provider config JSON and the environment overlay for the `opencode` backend |
| `docs/fork/opencode-probe-notes.md` | Observed facts about opencode 2.3.0 that the overlay relies on |
| `scripts/fork/check-opencode-overlay.sh` | Verifies files and hooks are present |
| `docs/fork/opencode-overlay.md` | This document |

## Hooks in upstream files

Every hook carries the marker `fork(opencode)`. Find them all with `grep -rn "fork(opencode)" crates`.

| File | Line(s) | Why | Re-apply if code moved |
| --- | --- | --- | --- |
| `crates/aionui-runtime/src/resolver.rs` | `crate::opencode_fork::return_if_overridden!(cmd); // fork(opencode)` | Make `resolve_command_path` honour the env override and the bundle for the command `opencode` | It must be the FIRST statement in the body of `resolve_command_path`, before `which::which(...)`. Every availability check and every spawn goes through this function; a later position would let the two disagree about which binary is used. The macro returns early only for the exact command `opencode`; every other command falls through unchanged. |
| `crates/aionui-runtime/src/lib.rs` | `pub mod opencode_fork; // fork(opencode)` | Declare module | Add next to the other `pub mod` lines |
| `crates/aionui-app/src/commands/mod.rs` | `pub(crate) mod opencode_bundle_fork; // fork(opencode)` | Declare module | Add to the `mod` list |
| `crates/aionui-app/src/commands/cmd_prepare_managed_resources.rs` | `let opencode_clis = super::opencode_bundle_fork::bundle_for_contract(&output_root).await?; // fork(opencode)` and `clis: opencode_clis, // fork(opencode)` | Put opencode into the bundle and into the contract | The first line goes right before the `ManagedResourcesContract { ... }` literal (after `output_root` and `runtime_key` exist); the second replaces the `clis: Vec::new()` field value in that literal. |
| `crates/aionui-ai-agent/src/factory/mod.rs` | `mod opencode_fork; // fork(opencode)` | Declare module | Add to the `mod` list |
| `crates/aionui-ai-agent/src/factory/acp.rs` | `super::opencode_fork::apply_opencode_env(&deps, &ctx, &config, &mut command_spec).await; // fork(opencode)` | Inject the opencode environment overlay | Immediately after the `apply_acp_launch_policy(...)` call and before `command_spec` is consumed (skill args, spawn). Before the policy call the overlay could be overwritten; after consumption it would have no effect. |
| `crates/aionui-runtime/Cargo.toml` | `wiremock.workspace = true # fork(opencode)` in `[dev-dependencies]` | Download tests | Re-add at the end of the section; if missing from the workspace table add it to the root `Cargo.toml` too |

## Mechanical / generated changes

| File | Handling |
| --- | --- |
| `Cargo.toml` (crate) | The `wiremock` dev-dependency above |
| `Cargo.lock` | One added `wiremock` entry in the `aionui-runtime` dependency list. Regenerate with a cargo build (`cargo build -p aionui-runtime`); never hand-merge. On conflict take upstream's version and rebuild. |

## Runtime contract

| Item | Value |
| --- | --- |
| Pinned version | `2.3.0` (`crates/aionui-runtime/resources/opencode-lock.json`) |
| Locked platforms | `linux-x64` (asset `opencode-linux-x64.tar.gz`) and `darwin-arm64` (asset `opencode-darwin-arm64.zip`) |
| Executable resolution | `AIONUI_OPENCODE_BIN` (absolute path to a regular file) → bundle (`cli_sources("opencode", <locked version>, <runtime key>)`) → PATH. Applied only when the command is exactly `opencode`; other commands resolve as before. The source (env override / bundle / PATH) is logged once per process at `info`; a PATH hit logs a `warn` that the pinned version is not in use. |
| Download base | `AIONUI_OPENCODE_DOWNLOAD_BASE` (default `https://github.com/anomalyco/opencode/releases/download`; `v<version>/<file>` is appended) |
| Download checks | SHA-256 of the archive is compared with the lock before extraction; `.tar.gz` and `.zip` assets are supported (regular files and directories only, no absolute or `..` paths) |
| Bundle layout | `<bundle root>/cli/opencode/<version>/<runtime_key>/opencode` |
| Backend | Overlay applies only to the agent whose backend is `opencode` |
| Managed provider | `newapi-managed` (from the newapi overlay); models with `model_enabled[id] == false` are excluded |
| With a usable provider | `OPENCODE_CONFIG_CONTENT` (JSON), `AIONUI_NEWAPI_KEY` (the key), `OPENCODE_DISABLE_AUTOUPDATE=1`, `OPENCODE_CONFIG_DIR=<data_dir>/opencode-config`, `OPENCODE_CONFIG=""`, `OPENCODE_DISABLE_PROJECT_CONFIG=1`, `OPENCODE_CONFIG_PROJECT_DISABLE=1` (alias, takes precedence) |
| Without one (fail closed) | A deny-all config, `OPENCODE_DISABLE_AUTOUPDATE=1`, `OPENCODE_CONFIG_DIR`, `OPENCODE_CONFIG=""`, `OPENCODE_DISABLE_PROJECT_CONFIG=1`, `OPENCODE_CONFIG_PROJECT_DISABLE=1`, and `AIONUI_NEWAPI_KEY=""` (empty, shadowing any key inherited from the shell or the backend environment) |
| Config JSON | `"update": "disable"`; provider id `newapi` with `"package": "@opencode/ai/providers/openai-compatible"` and `settings.baseURL` = managed provider `base_url`; `settings.apiKey` = `{env:AIONUI_NEWAPI_KEY}` (the key itself is never in the JSON); `policies` = deny `provider.use` on `*`, then allow `provider.use` on `newapi` |
| Model ids inside opencode | `newapi/<model id>` |
| Why `OPENCODE_CONFIG_DIR` | Without it opencode merges the user's `~/.config/opencode/opencode.json` (see probe notes). The overlay never writes to the user's home and does not touch `XDG_*`. |
| Why `OPENCODE_DISABLE_PROJECT_CONFIG` | opencode loads `<cwd>/opencode.json` and `<cwd>/.opencode/opencode.json` even with `OPENCODE_CONFIG_DIR` and `OPENCODE_CONFIG_CONTENT` set; a repository could declare its own provider plus an `allow` policy and receive prompts and code. The flag closes both locations (probe notes). Side effect: project agents and project MCP servers/instructions from project config are ignored too. |
| Inherited `OPENCODE_*` | Every other `OPENCODE_*` variable the child would inherit (from `agent_process_env()` or already in the spec) is set to an empty value, so a user's shell settings (`OPENCODE_FLAGS`, `OPENCODE_MODELS_URL`, `OPENCODE_CLI_CONFIG_CONTENT`, ...) are not visible to opencode. Only names are logged, at `info`. |
| Provider `enabled` flag | The managed provider row's own `enabled` flag is not consulted; only its key and per-model `model_enabled` are. |
| Logging | The key and the config JSON are never logged |

## Bumping the pinned opencode version

1. Edit `version` and the asset `sha256` (and `file` if the name changed) in `crates/aionui-runtime/resources/opencode-lock.json`. Update the version string in this document, the `LOCK_VERSION` constant in `scripts/fork/check-opencode-overlay.sh`, the hard-coded version and file name in the test `lock_parses_and_pins_2_3_0` (`crates/aionui-runtime/src/opencode_fork/mod.rs`), and the version reference in the AionUi-side doc (`AionUi/docs/fork/opencode-overlay.md`).
   Adding a platform: add an `assets` entry keyed by the runtime key (`current_runtime_key()` value) with `file`, `sha256` and `executable`; `.tar.gz` and `.zip` assets are supported.
2. Re-run every row of `docs/fork/opencode-probe-notes.md` against the new binary and update that file.
3. If the config format changed, edit only `crates/aionui-ai-agent/src/factory/opencode_fork.rs`.
4. Run `bash scripts/fork/check-opencode-overlay.sh` and the tests (`cargo test -p aionui-runtime`, `cargo test -p aionui-ai-agent`, `cargo test -p aionui-app`).
5. Run a full end-to-end check: produce a bundle (below), start AionCore with it, log in to newapi, create an opencode conversation, and confirm a prompt reaches the gateway.

## Producing and using a bundle in development

```
aioncore prepare-managed-resources --bundle-out <dir>
AIONUI_BUNDLED_MANAGED_RESOURCES=<dir> aioncore --managed-resources-mode bundled ...
```

The bundle root is only consulted in bundled mode: `--managed-resources-mode bundled` together with `AIONUI_BUNDLED_MANAGED_RESOURCES=<dir>` (the packaged desktop app passes that flag itself). Without the flag the env var is ignored and resolution falls through to PATH. When developing through the desktop app (which does not pass the flag when unpackaged), set `AIONUI_OPENCODE_BIN=<absolute path to the opencode binary>` instead.

`prepare-managed-resources` downloads the locked opencode into `<dir>/cli/opencode/<version>/<runtime_key>/` and lists it in the contract.

## Behaviour change for existing opencode users

opencode is also a built-in chat-tab agent (agent `53861a53`) and can be used by channel bots. **Every opencode process AionCore spawns now runs with the overlay**, not only the code tab:

- With a gateway sign-in, only newapi models are available.
- The user's global opencode config, `opencode auth login` credentials, plugins, project config in the workspace, and servers written by the upstream MCP sync into `~/.config/opencode` are ignored.
- Without a sign-in, `session/new` fails with "No models are available" (deny-all overlay).
- The user's own `OPENCODE_*` shell settings are ignored (blanked for spawned opencode).
- A resumed conversation whose stored model is not a newapi model falls back to the first model.

This follows from the requirement that opencode reaches only the gateway. Making the overlay conditional (for example only for the code tab) would be a product decision, not a code detail.

## Syncing with upstream

1. Record the old newapi base before touching anything: `old_newapi=$(git rev-parse feat/newapi-login)`.
2. Rebase the newapi overlay first, following `docs/fork/newapi-overlay.md`.
3. Then: `git rebase --onto feat/newapi-login "$old_newapi" feat/opencode-code-tab`.
4. Expected conflicts: only the hook lines above (and `Cargo.toml` / `Cargo.lock`). Re-add the one-liners from the hooks table; for `Cargo.lock` take upstream's and rebuild.
5. Run `bash scripts/fork/check-opencode-overlay.sh` and `bash scripts/fork/check-newapi-overlay.sh`.
6. `cargo test -p aionui-runtime -p aionui-ai-agent -p aionui-app`, `cargo clippy -p aionui-runtime -p aionui-ai-agent -p aionui-app --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
7. Update "Overlay base" at the top of this file to the new upstream commit.

## Commit layout

The branch is kept as three commits: (1) fork-owned files, (2) hooks and mechanical edits in upstream files (including `Cargo.toml` and `Cargo.lock`), (3) docs and check script. Conflicts during a rebase should only ever occur in commit (2).

## Known follow-ups

- Servers written by the upstream MCP sync (`OpencodeAdapter`) into `~/.config/opencode` no longer reach AionCore-spawned opencode because of `OPENCODE_CONFIG_DIR`.
- Only `linux-x64` and `darwin-arm64` are locked. On other platforms bundling is skipped with a warning. The probe notes were recorded on linux-x64 only; the `darwin-arm64` binary was bundled and detected but the probes were not re-run on macOS.
- Installer packaging and a fork release are not done.
- After a re-login a running opencode process keeps the old key until its runtime is restarted.
- opencode 2.x allows paths outside the project by default; no policy is added yet.
- The API key is visible to shell commands that opencode runs (inherent to env injection).
- The scratch directory used for downloads lives inside `--bundle-out` and can be left behind if the process is killed.
- The comment and `--help` text in `cmd_prepare_managed_resources.rs` still say no CLIs are bundled; left untouched to keep the upstream diff minimal.
