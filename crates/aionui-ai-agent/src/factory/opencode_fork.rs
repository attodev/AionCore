// fork(opencode): provider config and environment overlay for the bundled opencode — see docs/fork/opencode-overlay.md

use std::collections::HashMap;
use std::path::Path;
use std::sync::Once;

use aionui_api_types::AcpBuildExtra;
use aionui_common::{CommandSpec, EnvVar};
use aionui_db::IProviderRepository;
use aionui_runtime::opencode_fork::OPENCODE_COMMAND;
use serde_json::{Value, json};

use crate::factory::AgentFactoryDeps;
use crate::factory::context::FactoryContext;

pub const MANAGED_PROVIDER_ID: &str = "newapi-managed";
pub const OPENCODE_PROVIDER_ID: &str = "newapi";

const OPENCODE_BACKEND: &str = "opencode";
const KEY_ENV: &str = "AIONUI_NEWAPI_KEY";
const CONFIG_DIR_NAME: &str = "opencode-config";

/// Enabled models of the managed provider, in stored order.
pub struct OpencodeProviderInput {
    pub base_url: String,
    pub models: Vec<String>,
}

/// Pure: builds the opencode 2.x config that exposes only the managed provider.
/// `requested_model` accepts `newapi/<id>` or `<id>`; unknown values fall back to
/// the first model. Returns `None` when there is no model to offer.
pub fn build_opencode_config(input: &OpencodeProviderInput, requested_model: Option<&str>) -> Option<Value> {
    let first = input.models.first()?;
    let requested = requested_model.map(|m| {
        m.strip_prefix(OPENCODE_PROVIDER_ID)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(m)
    });
    let default = requested
        .and_then(|r| input.models.iter().find(|m| m.as_str() == r))
        .unwrap_or(first);
    if let Some(requested) = requested_model
        && !is_requested_default(requested, default)
    {
        tracing::info!(requested, chosen = %default, "requested opencode model not available; using the first model");
    }
    let models: serde_json::Map<String, Value> =
        input.models.iter().map(|m| (m.clone(), json!({ "name": m }))).collect();
    Some(json!({
        "$schema": "https://opencode.ai/config.json",
        "model": format!("{OPENCODE_PROVIDER_ID}/{default}"),
        "update": "disable",
        "policies": [
            { "action": "provider.use", "resource": "*", "effect": "deny" },
            { "action": "provider.use", "resource": OPENCODE_PROVIDER_ID, "effect": "allow" }
        ],
        "providers": {
            OPENCODE_PROVIDER_ID: {
                "name": "New API",
                "package": "@opencode/ai/providers/openai-compatible",
                "settings": { "baseURL": input.base_url, "apiKey": format!("{{env:{KEY_ENV}}}") },
                "models": models
            }
        }
    }))
}

/// True when `chosen` is what `requested` (`newapi/<id>` or `<id>`) asked for.
fn is_requested_default(requested: &str, chosen: &str) -> bool {
    let id = requested
        .strip_prefix(OPENCODE_PROVIDER_ID)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(requested);
    id == chosen
}

/// The key only travels in `AIONUI_NEWAPI_KEY`; the config JSON references it.
pub fn opencode_env(config: &Value, api_key: &str) -> Vec<EnvVar> {
    vec![
        EnvVar {
            name: "OPENCODE_CONFIG_CONTENT".into(),
            value: config.to_string(),
        },
        EnvVar {
            name: KEY_ENV.into(),
            value: api_key.to_owned(),
        },
        EnvVar {
            name: "OPENCODE_DISABLE_AUTOUPDATE".into(),
            value: "1".into(),
        },
    ]
}

/// Fail-closed config: no providers, every provider denied. Used when the managed
/// provider is unusable so opencode cannot reach its own cloud models.
pub fn deny_all_config() -> Value {
    json!({
        "$schema": "https://opencode.ai/config.json",
        "update": "disable",
        "policies": [{ "action": "provider.use", "resource": "*", "effect": "deny" }]
    })
}

fn overlay_inputs(config: &AcpBuildExtra) -> (Option<&str>, Option<&str>) {
    (config.backend.as_deref(), config.current_model_id.as_deref())
}

/// Injects the managed-provider config into an opencode spawn. No-op for other backends.
pub(super) async fn apply_opencode_env(
    deps: &AgentFactoryDeps,
    ctx: &FactoryContext,
    config: &AcpBuildExtra,
    spec: &mut CommandSpec,
) {
    let (backend, requested_model) = overlay_inputs(config);
    let inherited = if backend == Some(OPENCODE_BACKEND) {
        inherited_env_names().await
    } else {
        Vec::new()
    };
    apply_env(
        deps.provider_repo.as_ref(),
        &deps.encryption_key,
        &deps.data_dir,
        &ctx.user_id,
        backend,
        requested_model,
        &inherited,
        spec,
    )
    .await;
}

/// Names of the variables the spawned child would inherit (same source as the spawn).
async fn inherited_env_names() -> Vec<String> {
    aionui_runtime::agent_process_env()
        .await
        .into_iter()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect()
}

/// Replaces entries of `spec_env` with the overlay, then blanks every other `OPENCODE_*`
/// name found in `inherited` or already in `spec_env`. Returns the shadowed names.
fn finalize_env(spec_env: &mut Vec<EnvVar>, overlay: Vec<EnvVar>, inherited: &[String]) -> Vec<String> {
    // Windows environment names are case-insensitive (as in agent_env); Unix names are not.
    let norm = |name: &str| {
        if cfg!(windows) {
            name.to_ascii_uppercase()
        } else {
            name.to_owned()
        }
    };
    let overlay_names: Vec<String> = overlay.iter().map(|v| norm(&v.name)).collect();
    let mut candidates: Vec<String> = inherited.to_vec();
    candidates.extend(spec_env.iter().map(|e| e.name.clone()));

    let mut shadowed: Vec<String> = Vec::new();
    for name in candidates {
        let key = norm(&name);
        if key.starts_with("OPENCODE_") && !overlay_names.contains(&key) && !shadowed.iter().any(|s| norm(s) == key) {
            shadowed.push(name);
        }
    }
    for var in overlay {
        spec_env.retain(|e| norm(&e.name) != norm(&var.name));
        spec_env.push(var);
    }
    for name in &shadowed {
        spec_env.retain(|e| norm(&e.name) != norm(name));
        spec_env.push(EnvVar {
            name: name.clone(),
            value: String::new(),
        });
    }
    shadowed
}

// Explicit inputs keep the core testable without building AgentFactoryDeps.
#[allow(clippy::too_many_arguments)]
async fn apply_env(
    repo: &dyn IProviderRepository,
    encryption_key: &[u8; 32],
    data_dir: &Path,
    user_id: &str,
    backend: Option<&str>,
    requested_model: Option<&str>,
    inherited: &[String],
    spec: &mut CommandSpec,
) {
    if backend != Some(OPENCODE_BACKEND) {
        return;
    }
    log_binary_source_once();

    // Fail closed: the opencode backend always gets an overlay. Without a usable
    // managed provider it is deny-all, so neither the user's global config nor
    // opencode's own cloud models are reachable.
    let mut vars = match managed_overlay(repo, encryption_key, user_id, requested_model).await {
        Ok((config, api_key)) => opencode_env(&config, &api_key),
        Err(reason) => {
            tracing::warn!(reason, "opencode managed provider unusable; applying deny-all overlay");
            deny_all_env()
        }
    };
    // An empty config dir keeps opencode from merging the user's global config; it is
    // used even when it cannot be created (a missing directory holds no user config).
    let config_dir = data_dir.join(CONFIG_DIR_NAME);
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        tracing::warn!(error = %e, "cannot create opencode config dir");
    }
    vars.push(EnvVar {
        name: "OPENCODE_CONFIG_DIR".into(),
        value: config_dir.to_string_lossy().into_owned(),
    });
    // spec.env overrides inherited variables at spawn; blank a shell-level OPENCODE_CONFIG file.
    // A workspace's own opencode.json could add providers; ignore project config entirely.
    vars.push(EnvVar {
        name: "OPENCODE_DISABLE_PROJECT_CONFIG".into(),
        value: "1".into(),
    });
    // opencode also reads this alias, and it takes precedence over the variable above.
    vars.push(EnvVar {
        name: "OPENCODE_CONFIG_PROJECT_DISABLE".into(),
        value: "1".into(),
    });
    vars.push(EnvVar {
        name: "OPENCODE_CONFIG".into(),
        value: String::new(),
    });
    // Since 2.3.1 opencode puts the user's ~/.claude/CLAUDE.md into the prompt when the
    // (empty) config dir has no AGENTS.md; personal instructions must not reach the gateway.
    vars.push(EnvVar {
        name: "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT".into(),
        value: "1".into(),
    });
    let shadowed = finalize_env(&mut spec.env, vars, inherited);
    if !shadowed.is_empty() {
        tracing::info!(names = ?shadowed, "shadowed inherited OPENCODE_* variables for opencode");
    }
}

fn deny_all_env() -> Vec<EnvVar> {
    vec![
        EnvVar {
            name: "OPENCODE_CONFIG_CONTENT".into(),
            value: deny_all_config().to_string(),
        },
        // Shadows a key inherited from the shell or the backend's own environment.
        EnvVar {
            name: KEY_ENV.into(),
            value: String::new(),
        },
        EnvVar {
            name: "OPENCODE_DISABLE_AUTOUPDATE".into(),
            value: "1".into(),
        },
    ]
}

/// The config and decrypted key for the managed provider, or a fixed reason it is unusable.
async fn managed_overlay(
    repo: &dyn IProviderRepository,
    encryption_key: &[u8; 32],
    user_id: &str,
    requested_model: Option<&str>,
) -> Result<(Value, String), &'static str> {
    let row = match repo.find_by_id(user_id, MANAGED_PROVIDER_ID).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err("no managed provider"),
        Err(_) => return Err("provider lookup failed"),
    };
    let api_key = match aionui_common::decrypt_string(&row.api_key_encrypted, encryption_key) {
        Ok(key) if !key.trim().is_empty() => key,
        Ok(_) => return Err("managed provider key is blank"),
        Err(_) => return Err("managed provider key could not be decrypted"),
    };
    let input = OpencodeProviderInput {
        base_url: row.base_url.clone(),
        models: enabled_models(&row.models, row.model_enabled.as_deref()),
    };
    let config = build_opencode_config(&input, requested_model).ok_or("managed provider has no enabled models")?;
    Ok((config, api_key))
}

fn enabled_models(models_json: &str, enabled_json: Option<&str>) -> Vec<String> {
    let models: Vec<String> = serde_json::from_str(models_json).unwrap_or_default();
    let enabled: HashMap<String, bool> = enabled_json
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    models
        .into_iter()
        .filter(|m| !m.is_empty() && enabled.get(m) != Some(&false))
        .collect()
}

fn log_binary_source_once() {
    use aionui_runtime::opencode_fork::{OpencodeSource, override_source};
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| match override_source(OPENCODE_COMMAND) {
        Some((path, OpencodeSource::EnvOverride)) => {
            tracing::info!(path = %path.display(), "opencode binary from AIONUI_OPENCODE_BIN");
        }
        Some((path, OpencodeSource::Bundled)) => {
            tracing::info!(path = %path.display(), "opencode binary from the bundled pinned version");
        }
        None => tracing::warn!("using the opencode found on PATH, not the pinned bundled version"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_db::CreateProviderParams;
    use aionui_db::models::Provider;
    use aionui_db::{SqliteProviderRepository, init_database_memory};
    use serde_json::json;

    const USER: &str = "system_default_user";
    const KEY: [u8; 32] = [7u8; 32];
    const SECRET: &str = "sk-super-secret-key";

    fn input(models: &[&str]) -> OpencodeProviderInput {
        OpencodeProviderInput {
            base_url: "http://h/v1".into(),
            models: models.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    fn env_value<'a>(env: &'a [EnvVar], name: &str) -> Option<&'a str> {
        env.iter().find(|e| e.name == name).map(|e| e.value.as_str())
    }

    #[test]
    fn config_matches_contract() {
        let cfg = build_opencode_config(&input(&["gpt-4o", "gpt-4o-mini"]), None).unwrap();
        assert_eq!(cfg["model"], "newapi/gpt-4o");
        assert_eq!(cfg["update"], "disable");
        assert_eq!(
            cfg["policies"],
            json!([
                {"action":"provider.use","resource":"*","effect":"deny"},
                {"action":"provider.use","resource":"newapi","effect":"allow"}
            ])
        );
        let p = &cfg["providers"]["newapi"];
        assert_eq!(p["package"], "@opencode/ai/providers/openai-compatible");
        assert_eq!(p["settings"]["baseURL"], "http://h/v1");
        assert_eq!(p["settings"]["apiKey"], "{env:AIONUI_NEWAPI_KEY}");
        let models = p["models"].as_object().unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.contains_key("gpt-4o") && models.contains_key("gpt-4o-mini"));
    }

    #[test]
    fn requested_model_selected_when_listed() {
        let i = input(&["gpt-4o", "gpt-4o-mini"]);
        for req in ["newapi/gpt-4o-mini", "gpt-4o-mini"] {
            let cfg = build_opencode_config(&i, Some(req)).unwrap();
            assert_eq!(cfg["model"], "newapi/gpt-4o-mini");
        }
        let cfg = build_opencode_config(&i, Some("nope")).unwrap();
        assert_eq!(cfg["model"], "newapi/gpt-4o");
    }

    #[test]
    fn odd_model_ids_survive() {
        let cfg = build_opencode_config(&input(&["vendor/model x", "모델-1", "a\"b"]), None).unwrap();
        let back: Value = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        let models = back["providers"]["newapi"]["models"].as_object().unwrap();
        for id in ["vendor/model x", "모델-1", "a\"b"] {
            assert!(models.contains_key(id), "missing {id}");
        }
        assert_eq!(back["model"], "newapi/vendor/model x");
    }

    #[test]
    fn requested_default_detection() {
        assert!(is_requested_default("newapi/a", "a"));
        assert!(is_requested_default("a", "a"));
        assert!(!is_requested_default("newapi/b", "a"));
        assert!(!is_requested_default("b", "a"));
    }

    #[test]
    fn no_models_means_no_config() {
        assert!(build_opencode_config(&input(&[]), None).is_none());
    }

    #[test]
    fn env_never_contains_key_in_config() {
        let cfg = build_opencode_config(&input(&["m"]), None).unwrap();
        let env = opencode_env(&cfg, SECRET);
        assert!(!env_value(&env, "OPENCODE_CONFIG_CONTENT").unwrap().contains(SECRET));
        assert_eq!(env_value(&env, "AIONUI_NEWAPI_KEY"), Some(SECRET));
        assert_eq!(env_value(&env, "OPENCODE_DISABLE_AUTOUPDATE"), Some("1"));
    }

    struct Fixture {
        repo: SqliteProviderRepository,
        dir: tempfile::TempDir,
        _db: aionui_db::Database,
    }

    async fn fixture() -> Fixture {
        let db = init_database_memory().await.unwrap();
        let repo = SqliteProviderRepository::new(db.pool().clone());
        Fixture {
            repo,
            dir: tempfile::tempdir().unwrap(),
            _db: db,
        }
    }

    async fn add_provider(f: &Fixture, key: &str, models: &str, model_enabled: Option<&str>) -> Provider {
        let enc = aionui_common::encrypt_string(key, &KEY).unwrap();
        add_provider_with_encrypted(f, &enc, models, model_enabled).await
    }

    async fn add_provider_with_encrypted(
        f: &Fixture,
        enc: &str,
        models: &str,
        model_enabled: Option<&str>,
    ) -> Provider {
        use aionui_db::IProviderRepository as _;
        f.repo
            .create(CreateProviderParams {
                id: Some(MANAGED_PROVIDER_ID),
                user_id: USER,
                platform: "custom",
                name: "New API",
                base_url: "http://h/v1",
                api_key_encrypted: enc,
                models,
                enabled: true,
                capabilities: "[]",
                context_limit: None,
                model_protocols: None,
                model_enabled,
                model_health: None,
                model_settings: "{}",
                bedrock_config: None,
                is_full_url: false,
            })
            .await
            .unwrap()
    }

    async fn run(f: &Fixture, backend: Option<&str>, model: Option<&str>, spec: &mut CommandSpec) {
        apply_env(&f.repo, &KEY, f.dir.path(), USER, backend, model, &[], spec).await;
    }

    fn deny_all_json() -> Value {
        json!({
            "$schema": "https://opencode.ai/config.json",
            "update": "disable",
            "policies": [{"action":"provider.use","resource":"*","effect":"deny"}]
        })
    }

    /// Asserts the fail-closed overlay: deny-all content, no key entry, config dir + empty OPENCODE_CONFIG.
    fn assert_deny_all(f: &Fixture, spec: &CommandSpec) {
        let cfg: Value = serde_json::from_str(env_value(&spec.env, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        assert_eq!(cfg, deny_all_json());
        assert_eq!(env_value(&spec.env, "AIONUI_NEWAPI_KEY"), Some(""));
        assert_eq!(spec.env.iter().filter(|e| e.name == "AIONUI_NEWAPI_KEY").count(), 1);
        assert_eq!(env_value(&spec.env, "OPENCODE_DISABLE_AUTOUPDATE"), Some("1"));
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG"), Some(""));
        assert_eq!(env_value(&spec.env, "OPENCODE_DISABLE_PROJECT_CONFIG"), Some("1"));
        assert_eq!(env_value(&spec.env, "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT"), Some("1"));
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG_PROJECT_DISABLE"), Some("1"));
        let dir = f.dir.path().join("opencode-config");
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG_DIR"), Some(dir.to_str().unwrap()));
    }

    #[test]
    fn deny_all_config_matches_ruling() {
        assert_eq!(deny_all_config(), deny_all_json());
    }

    #[test]
    fn overlay_inputs_forwards_backend_and_model() {
        let cfg = AcpBuildExtra {
            backend: Some("opencode".into()),
            current_model_id: Some("newapi/x".into()),
            ..Default::default()
        };
        assert_eq!(overlay_inputs(&cfg), (Some("opencode"), Some("newapi/x")));
        assert_eq!(overlay_inputs(&AcpBuildExtra::default()), (None, None));
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn finalize_env_blanks_inherited_opencode_vars_but_not_the_overlay() {
        let cfg = build_opencode_config(&input(&["m"]), None).unwrap();
        let mut overlay = opencode_env(&cfg, SECRET);
        for (n, v) in [
            ("OPENCODE_CONFIG_PROJECT_DISABLE", "1"),
            ("OPENCODE_DISABLE_PROJECT_CONFIG", "1"),
        ] {
            overlay.push(EnvVar {
                name: n.into(),
                value: v.into(),
            });
        }
        let mut spec_env = vec![
            EnvVar {
                name: "OPENCODE_FLAGS".into(),
                value: "x".into(),
            },
            EnvVar {
                name: "UNRELATED".into(),
                value: "keep".into(),
            },
        ];
        let inherited = names(&[
            "OPENCODE_CONFIG_PROJECT_DISABLE",
            "OPENCODE_FLAGS",
            "OPENCODE_MODELS_URL",
            "OPENCODE_CLI_CONFIG_CONTENT",
            "OPENCODE_CONFIG_CONTENT",
            "HOME",
        ]);
        let mut shadowed = finalize_env(&mut spec_env, overlay, &inherited);
        shadowed.sort();
        assert_eq!(
            shadowed,
            ["OPENCODE_CLI_CONFIG_CONTENT", "OPENCODE_FLAGS", "OPENCODE_MODELS_URL"]
        );
        assert_eq!(env_value(&spec_env, "OPENCODE_CONFIG_PROJECT_DISABLE"), Some("1"));
        assert_eq!(env_value(&spec_env, "OPENCODE_DISABLE_PROJECT_CONFIG"), Some("1"));
        assert_eq!(env_value(&spec_env, "AIONUI_NEWAPI_KEY"), Some(SECRET));
        assert_ne!(env_value(&spec_env, "OPENCODE_CONFIG_CONTENT"), Some(""));
        for n in ["OPENCODE_FLAGS", "OPENCODE_MODELS_URL", "OPENCODE_CLI_CONFIG_CONTENT"] {
            assert_eq!(env_value(&spec_env, n), Some(""), "{n}");
        }
        assert_eq!(env_value(&spec_env, "UNRELATED"), Some("keep"));
        assert!(env_value(&spec_env, "HOME").is_none());
        for n in ["OPENCODE_FLAGS", "OPENCODE_CONFIG_PROJECT_DISABLE"] {
            assert_eq!(spec_env.iter().filter(|e| e.name == n).count(), 1);
        }
    }

    #[tokio::test]
    async fn apply_env_shadows_inherited_names_end_to_end() {
        let f = fixture().await;
        add_provider(&f, SECRET, r#"["a"]"#, None).await;
        let mut spec = CommandSpec::default();
        let inherited = names(&["OPENCODE_CONFIG_PROJECT_DISABLE", "OPENCODE_FLAGS", "PATH"]);
        apply_env(
            &f.repo,
            &KEY,
            f.dir.path(),
            USER,
            Some("opencode"),
            None,
            &inherited,
            &mut spec,
        )
        .await;
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG_PROJECT_DISABLE"), Some("1"));
        assert_eq!(env_value(&spec.env, "OPENCODE_FLAGS"), Some(""));
        assert!(env_value(&spec.env, "PATH").is_none());
    }

    #[tokio::test]
    async fn apply_skips_other_backends() {
        let f = fixture().await;
        add_provider(&f, SECRET, r#"["a"]"#, None).await;
        for backend in [Some("claude"), None] {
            let mut spec = CommandSpec::default();
            run(&f, backend, None, &mut spec).await;
            assert!(spec.env.is_empty());
        }
        assert!(!f.dir.path().join("opencode-config").exists());
    }

    #[tokio::test]
    async fn apply_injects_for_opencode() {
        let f = fixture().await;
        add_provider(&f, SECRET, r#"["a","b","c"]"#, Some(r#"{"b":false,"a":true}"#)).await;
        let mut spec = CommandSpec::default();
        run(&f, Some("opencode"), Some("newapi/c"), &mut spec).await;
        assert_eq!(spec.env.len(), 8);
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG_PROJECT_DISABLE"), Some("1"));
        assert_eq!(env_value(&spec.env, "OPENCODE_DISABLE_PROJECT_CONFIG"), Some("1"));
        assert_eq!(env_value(&spec.env, "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT"), Some("1"));
        let cfg: Value = serde_json::from_str(env_value(&spec.env, "OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        let models = cfg["providers"]["newapi"]["models"].as_object().unwrap();
        assert_eq!(models.keys().collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(cfg["model"], "newapi/c");
        assert_eq!(env_value(&spec.env, "AIONUI_NEWAPI_KEY"), Some(SECRET));
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG"), Some(""));
        let cfg_dir = f.dir.path().join("opencode-config");
        assert_eq!(
            env_value(&spec.env, "OPENCODE_CONFIG_DIR"),
            Some(cfg_dir.to_str().unwrap())
        );
        assert!(cfg_dir.is_dir());
        assert_eq!(std::fs::read_dir(&cfg_dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn apply_overrides_existing_env() {
        let f = fixture().await;
        add_provider(&f, SECRET, r#"["a"]"#, None).await;
        let mut spec = CommandSpec::default();
        for (name, value) in [
            ("OPENCODE_CONFIG_CONTENT", "user"),
            ("OPENCODE_CONFIG", "/u.json"),
            ("KEEP", "1"),
        ] {
            spec.env.push(EnvVar {
                name: name.into(),
                value: value.into(),
            });
        }
        run(&f, Some("opencode"), None, &mut spec).await;
        for name in [
            "OPENCODE_CONFIG_CONTENT",
            "OPENCODE_CONFIG",
            "OPENCODE_DISABLE_PROJECT_CONFIG",
        ] {
            assert_eq!(spec.env.iter().filter(|e| e.name == name).count(), 1);
        }
        assert_ne!(env_value(&spec.env, "OPENCODE_CONFIG_CONTENT"), Some("user"));
        assert_eq!(env_value(&spec.env, "OPENCODE_CONFIG"), Some(""));
        assert_eq!(env_value(&spec.env, "KEEP"), Some("1"));
    }

    async fn assert_fails_closed(f: &Fixture) {
        let mut spec = CommandSpec::default();
        spec.env.push(EnvVar {
            name: "AIONUI_NEWAPI_KEY".into(),
            value: "inherited".into(),
        });
        spec.env.push(EnvVar {
            name: "OPENCODE_CONFIG_CONTENT".into(),
            value: "user".into(),
        });
        run(f, Some("opencode"), None, &mut spec).await;
        assert_deny_all(f, &spec);
        assert!(!format!("{:?}", spec.env).contains("inherited"));
    }

    struct FailingRepo;

    #[async_trait::async_trait]
    impl IProviderRepository for FailingRepo {
        async fn list(&self, _: &str) -> Result<Vec<Provider>, aionui_db::DbError> {
            Err(aionui_db::DbError::Init("boom".into()))
        }
        async fn find_by_id(&self, _: &str, _: &str) -> Result<Option<Provider>, aionui_db::DbError> {
            Err(aionui_db::DbError::Init("boom".into()))
        }
        async fn create(&self, _: CreateProviderParams<'_>) -> Result<Provider, aionui_db::DbError> {
            unreachable!()
        }
        async fn update(
            &self,
            _: &str,
            _: &str,
            _: aionui_db::UpdateProviderParams<'_>,
        ) -> Result<Provider, aionui_db::DbError> {
            unreachable!()
        }
        async fn delete(&self, _: &str, _: &str) -> Result<(), aionui_db::DbError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn apply_denies_all_when_lookup_fails() {
        let f = fixture().await;
        let mut spec = CommandSpec::default();
        apply_env(
            &FailingRepo,
            &KEY,
            f.dir.path(),
            USER,
            Some("opencode"),
            None,
            &[],
            &mut spec,
        )
        .await;
        assert_deny_all(&f, &spec);
    }

    #[tokio::test]
    async fn apply_denies_all_when_no_provider() {
        let f = fixture().await;
        assert_fails_closed(&f).await;
    }

    #[tokio::test]
    async fn apply_denies_all_when_blank_key() {
        let f = fixture().await;
        add_provider(&f, "  ", r#"["a"]"#, None).await;
        assert_fails_closed(&f).await;
    }

    #[tokio::test]
    async fn apply_denies_all_when_no_enabled_models() {
        let f = fixture().await;
        add_provider(&f, SECRET, r#"["a"]"#, Some(r#"{"a":false}"#)).await;
        assert_fails_closed(&f).await;
    }

    #[tokio::test]
    async fn apply_denies_all_on_invalid_model_json() {
        let f = fixture().await;
        add_provider(&f, SECRET, "not json", Some("{{{")).await;
        assert_fails_closed(&f).await;
    }

    #[tokio::test]
    async fn apply_denies_all_on_decrypt_failure() {
        let f = fixture().await;
        let enc = aionui_common::encrypt_string(SECRET, &[9u8; 32]).unwrap();
        add_provider_with_encrypted(&f, &enc, r#"["a"]"#, None).await;
        assert_fails_closed(&f).await;
        let mut spec = CommandSpec::default();
        run(&f, Some("opencode"), None, &mut spec).await;
        assert!(!format!("{:?}", spec.env).contains(SECRET));
    }

    #[tokio::test]
    async fn apply_still_injects_when_config_dir_cannot_be_created() {
        let f = fixture().await;
        add_provider(&f, SECRET, r#"["a"]"#, None).await;
        // A regular file where the data dir should be makes create_dir_all fail.
        let blocker = f.dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let mut spec = CommandSpec::default();
        apply_env(&f.repo, &KEY, &blocker, USER, Some("opencode"), None, &[], &mut spec).await;
        let expected = blocker.join("opencode-config");
        assert_eq!(
            env_value(&spec.env, "OPENCODE_CONFIG_DIR"),
            Some(expected.to_str().unwrap())
        );
        assert!(env_value(&spec.env, "OPENCODE_CONFIG_CONTENT").is_some());
        assert!(!expected.exists());
    }
}
