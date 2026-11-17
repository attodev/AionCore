# opencode 2.3.0 probe notes

Recorded 2026-11-17 on linux-x64 against a local new-api gateway with a mock upstream.
These are observations the opencode overlay relies on. Re-run them whenever the pinned
version in `crates/aionui-runtime/resources/opencode-lock.json` changes.

| Question | Result |
|---|---|
| Pinned asset | `opencode-linux-x64.tar.gz` from `https://github.com/anomalyco/opencode/releases/download/v2.3.0/`; the archive contains one file, `opencode` |
| SHA-256 of the asset | `fc0f81522aceeabac47693d88389efa2bc517ce74c2c38fd6967819fcc6e4d3b` |
| `current_runtime_key()` on this host | `linux-x64` |
| Config injection | `OPENCODE_CONFIG_CONTENT` (inline JSON) is honoured; the native 2.x shape (`providers`, `settings`, `policies`, `update`) works |
| Provider restriction | With the two `provider.use` policies (deny `*`, allow `newapi`) the ACP model option lists only `newapi/<id>` models. Without them a dozen `opencode/…-free` cloud models are listed |
| User's global config | With `OPENCODE_CONFIG_CONTENT` alone, `~/.config/opencode/opencode.json` IS merged (a provider defined there shows up in the model list). With `OPENCODE_CONFIG_DIR` pointing at an empty directory it is NOT merged. The global file is never modified in either case. **The overlay must set `OPENCODE_CONFIG_DIR`.** |
| Key on disk | Not written anywhere under the opencode data/config/cache/state directories when passed via `{env:AIONUI_NEWAPI_KEY}` |
| Extra downloads | None; the OpenAI-compatible provider is built in |
| Model override | Creating a conversation with `conversation_overrides.model = "newapi/gpt-4o-mini"` makes the gateway receive `gpt-4o-mini` |
| Mode override | `conversation_overrides.permission = "plan"` yields `session_mode = plan`, `current_mode_id = plan`; ACP mode ids are `build` and `plan` |
| AionCore integration | AionCore (built from this branch's base) drives 2.3.0 through `opencode acp` end to end with no warnings or errors in its log |
| Project config escape | opencode loads `<cwd>/opencode.json` AND `<cwd>/.opencode/opencode.json` even with `OPENCODE_CONFIG_DIR` and `OPENCODE_CONFIG_CONTENT` set. A project file declaring provider `evil` plus an `allow` policy for it: ACP `session/new` model options under the managed overlay are `['evil/m', 'newapi/a']`; under the deny-all overlay `['evil/m']` (both locations) |
| `OPENCODE_DISABLE_PROJECT_CONFIG=1` | Closes both locations for both overlays: managed -> `['newapi/a']`; deny-all -> `session/new` error `No models are available` (`errorName: CatalogNotReady`). `debug config` no longer lists the project `opencode.json` or the `.opencode` directory |
| What else the flag disables | Project `agents/*.md` in `.opencode/` (an agent `pagent` disappears from `debug agents`), and, via project config, MCP servers and `instructions` entries (no longer in `debug config`). Not confirmed: project `AGENTS.md` loading, `.opencode/commands` and `.opencode/plugins` (no inspection command shows them; a plugin file was present in the workspace and nothing in `debug` output reflected it either way) |
| Provider keys in env | Under deny-all (flag set, empty workspace), `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` in the environment do not bring models back: `session/new` still fails with `No models are available` |
| Deny-all acceptance | The deny-all JSON (`$schema`, `update`, one deny `provider.use` `*` policy) parses without error; `debug config` shows the policy and no `providers` or `model` |
| Probe caveat | `debug config` / `debug agents` talk to a managed `opencode serve --service` that is reused across invocations with the first caller's environment; kill it between runs (and after the probe) or later results reflect the first run's config |
| Project-config alias | opencode 2.3.0 also reads `OPENCODE_CONFIG_PROJECT_DISABLE`, which takes precedence over `OPENCODE_DISABLE_PROJECT_CONFIG`. Inherited with a non-true value (`0`, `false`, empty) it re-enables project config despite the overlay's `OPENCODE_DISABLE_PROJECT_CONFIG=1` (managed -> `['evil/m','newapi/a']`, deny-all -> `['evil/m']`). With the alias `1` it is closed. Verified again: inherited alias `0` without the overlay setting it -> `['evil/m','newapi/a']` |
| Other routes (re-review, verified closed) | `OPENCODE_CLI_CONFIG_CONTENT`, a parent-directory `opencode.json`, and `OPENCODE_TEST_HOME` are closed by the overlay (project-config flag plus `OPENCODE_CONFIG_DIR`) |
| Blanked `OPENCODE_*` | With the final overlay values plus `OPENCODE_FLAGS=`, `OPENCODE_MODELS_URL=` and `OPENCODE_CLI_CONFIG_CONTENT=` set EMPTY: opencode starts; managed -> only `newapi/a`; deny-all -> `No models are available`. Alias `1` alone (flag unset) also closes the workspace `opencode.json` in both overlays. Empty values for these three break nothing |
| Still unverified | `/etc/opencode` system config; semantics of `OPENCODE_FLAGS` and `OPENCODE_MODELS_URL` / `OPENCODE_MODELS_PATH` (they are blanked, not understood) |

Not covered: Windows and macOS, tool calls with a real model, permission prompts, session
resume, MCP servers passed through ACP.
