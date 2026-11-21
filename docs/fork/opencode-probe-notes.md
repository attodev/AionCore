# opencode probe notes (pinned: 2.3.2)

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

## 2.3.2 re-check (2026-11-21, darwin-arm64)

The pin moved from 2.3.0 to 2.3.2. The rows above were recorded for 2.3.0 on linux-x64 and were not all repeated. What was re-run, on macOS arm64 with the final overlay environment, a throwaway `HOME`, and a local stub in place of the gateway, driving `opencode acp` (`initialize`, `session/new`, `session/prompt`); every row was run against both 2.3.0 and 2.3.2:

| Question | 2.3.2 result |
|---|---|
| Pinned assets | `opencode-linux-x64.tar.gz` SHA-256 `563b5d663637226b6e3bb4bb6803281bc39b0d2cd88db06d674b6420aab009b0`; `opencode-darwin-arm64.zip` SHA-256 `31ab5a06a29edc7e49b895f35c2b64ebfe7c7f0437b0f2581b006efcd79fee1a`. Both downloaded and hashed locally; they match the digests GitHub publishes for the release. `opencode --version` prints `v2.3.2` |
| Provider restriction | Managed overlay: model options are `['newapi/a']`. A prompt produces `POST /v1/chat/completions` on the configured base URL with `Authorization: Bearer <AIONUI_NEWAPI_KEY>` and nothing else. Same as 2.3.0 |
| User's global config | A `~/.config/opencode/opencode.json` declaring provider `evil` plus an `allow` policy is not merged: `['newapi/a']`. Same as 2.3.0 |
| Project config | `<cwd>/opencode.json` and `<cwd>/.opencode/opencode.json` declaring `evil`: `['newapi/a']`. Same as 2.3.0 |
| Deny-all | `session/new` fails with `No models are available` (`errorName: CatalogNotReady`), also with project and global `evil` configs present and `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` set. Same as 2.3.0 |
| Blanked `OPENCODE_*` | `OPENCODE_FLAGS=`, `OPENCODE_MODELS_URL=`, `OPENCODE_CLI_CONFIG_CONTENT=` empty: starts, `['newapi/a']`. Same as 2.3.0 |
| `~/.claude/CLAUDE.md` (**changed**) | 2.3.2 puts the content of `~/.claude/CLAUDE.md` into the request sent to the gateway when the config dir has no `AGENTS.md`; 2.3.0 did not. With `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1` (or `OPENCODE_DISABLE_CLAUDE_CODE=1`) it is not sent. **The overlay must set `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT`.** |
| Workspace instruction files | With the overlay environment, neither `<cwd>/CLAUDE.md` nor `<cwd>/AGENTS.md` content appears in the request, on 2.3.0 and 2.3.2 alike. An `AGENTS.md` placed in `OPENCODE_CONFIG_DIR` is sent (the overlay keeps that directory empty); `~/.config/opencode/AGENTS.md` is not |

Not re-run for 2.3.2: everything on linux-x64, the AionCore end-to-end rows (model and mode override through a real conversation), "Key on disk", "Extra downloads", `debug config` / `debug agents` checks, and the "Other routes" row. 2.3.1 also changed defaults the overlay does not control: a shell command that outlives its timeout now continues in the background, and compaction summaries are written in the background (`compaction.background`).

Not covered: Windows and macOS, tool calls with a real model, permission prompts, session
resume, MCP servers passed through ACP.
