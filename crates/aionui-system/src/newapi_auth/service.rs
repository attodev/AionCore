// fork(newapi): managed login service, managed provider upsert and authChanged event — see docs/fork/newapi-overlay.md
//! Managed New API login service: status decision, pending PKCE login,
//! managed-provider upsert and the `newapi.authChanged` realtime event.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aionui_api_types::{
    CreateProviderRequest, ModelOpenAiApiMode, ModelSettings, NewApiAuthChangedEvent, NewApiAuthReason,
    NewApiAuthState, NewApiLoginStartResponse, NewApiStatusResponse, ProviderResponse, UpdateProviderRequest,
    WebSocketMessage,
};
use aionui_realtime::EventBroadcaster;

use super::client::{NewApiClient, ProbeOutcome};
use super::config::{self, BUILT_IN_DEFAULT_URL, CONFIG_FILE_NAME, ENV_BASE_URL};
use super::pkce;
use crate::error::SystemError;
use crate::provider::ProviderService;

pub const MANAGED_PROVIDER_ID: &str = "newapi-managed";
pub const EVENT_AUTH_CHANGED: &str = "newapi.authChanged";
pub const PENDING_TTL: Duration = Duration::from_secs(300);

const MANAGED_PLATFORM: &str = "new-api";
const MANAGED_NAME: &str = "New API";
const MSG_EXPIRED: &str = "The sign-in request expired. Start again from the app.";

/// URL sources: `Default` reads the process env / build-time default on each call.
#[derive(Clone)]
enum UrlSources {
    Default,
    Fixed {
        env: Option<String>,
        built_in: Option<String>,
    },
}

struct PendingLogin {
    state: String,
    verifier: String,
    root: String,
    port: u16,
    created: Instant,
}

#[derive(Clone)]
pub struct NewApiAuthService {
    provider_service: ProviderService,
    client: NewApiClient,
    broadcaster: Arc<dyn EventBroadcaster>,
    data_dir: PathBuf,
    url_sources: UrlSources,
    pending: Arc<Mutex<Option<PendingLogin>>>,
    username: Arc<Mutex<Option<String>>>,
}

impl NewApiAuthService {
    pub fn new(
        provider_service: ProviderService,
        client: NewApiClient,
        broadcaster: Arc<dyn EventBroadcaster>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            provider_service,
            client,
            broadcaster,
            data_dir,
            url_sources: UrlSources::Default,
            pending: Arc::new(Mutex::new(None)),
            username: Arc::new(Mutex::new(None)),
        }
    }

    /// Pins the env / built-in addresses (tests must not depend on the process env).
    pub fn with_url_sources(mut self, env: Option<String>, built_in: Option<String>) -> Self {
        self.url_sources = UrlSources::Fixed { env, built_in };
        self
    }

    fn resolve_root(&self) -> Option<String> {
        match &self.url_sources {
            UrlSources::Default => {
                let env = std::env::var(ENV_BASE_URL).ok();
                config::resolve_base_url(&self.data_dir, env.as_deref(), BUILT_IN_DEFAULT_URL)
            }
            UrlSources::Fixed { env, built_in } => {
                config::resolve_base_url(&self.data_dir, env.as_deref(), built_in.as_deref())
            }
        }
    }

    fn config_path(&self) -> String {
        self.data_dir.join(CONFIG_FILE_NAME).display().to_string()
    }

    fn status_response(
        &self,
        state: NewApiAuthState,
        root: Option<String>,
        reason: Option<NewApiAuthReason>,
    ) -> NewApiStatusResponse {
        let username = if state == NewApiAuthState::Ready {
            self.username.lock().expect("username lock poisoned").clone()
        } else {
            None
        };
        NewApiStatusResponse {
            state,
            base_url: root,
            reason,
            username,
            config_path: self.config_path(),
        }
    }

    pub async fn status(&self, user_id: &str) -> Result<NewApiStatusResponse, SystemError> {
        let Some(root) = self.resolve_root() else {
            return Ok(self.status_response(NewApiAuthState::NotConfigured, None, None));
        };
        let provider = self.find_managed(user_id).await?;
        let Some(provider) = provider.filter(|p| !p.api_key.trim().is_empty()) else {
            return Ok(self.status_response(NewApiAuthState::NeedsLogin, Some(root), Some(NewApiAuthReason::NoToken)));
        };
        match self.client.probe_models(&root, &provider.api_key).await {
            ProbeOutcome::Unauthorized => Ok(self.status_response(
                NewApiAuthState::NeedsLogin,
                Some(root),
                Some(NewApiAuthReason::TokenRejected),
            )),
            ProbeOutcome::Unreachable => {
                Ok(self.status_response(NewApiAuthState::Ready, Some(root), Some(NewApiAuthReason::Offline)))
            }
            ProbeOutcome::Models(ids) => self.refresh_models(user_id, root, &provider.api_key, ids).await,
        }
    }

    /// Status-path write: only `models`, `model_settings` and `base_url`, never the key or
    /// `enabled`. Re-reads the provider first so a login or delete that raced the probe wins.
    async fn refresh_models(
        &self,
        user_id: &str,
        root: String,
        probed_key: &str,
        ids: Vec<String>,
    ) -> Result<NewApiStatusResponse, SystemError> {
        let needs_login_no_token = |svc: &Self, root: String| {
            svc.status_response(NewApiAuthState::NeedsLogin, Some(root), Some(NewApiAuthReason::NoToken))
        };
        let Some(current) = self.find_managed(user_id).await? else {
            return Ok(needs_login_no_token(self, root));
        };
        if current.api_key.trim().is_empty() {
            return Ok(needs_login_no_token(self, root));
        }
        // The probe result belongs to a superseded key: do not write it over the new login.
        if current.api_key != probed_key {
            return Ok(self.status_response(NewApiAuthState::Ready, Some(root), None));
        }
        let base_url = config::provider_base_url(&root);
        let model_settings = pinned_model_settings(&ids, Some(&current));
        if current.base_url != base_url || current.models != ids || current.model_settings != model_settings {
            let update = self
                .provider_service
                .update(
                    user_id,
                    MANAGED_PROVIDER_ID,
                    UpdateProviderRequest {
                        base_url: Some(base_url),
                        models: Some(ids),
                        model_settings: Some(model_settings),
                        ..Default::default()
                    },
                )
                .await;
            match update {
                Ok(_) => {}
                Err(SystemError::NotFound(_)) => return Ok(needs_login_no_token(self, root)),
                Err(e) => return Err(e),
            }
        }
        Ok(self.status_response(NewApiAuthState::Ready, Some(root), None))
    }

    pub fn start_login(&self, port: u16) -> Result<NewApiLoginStartResponse, SystemError> {
        let root = self
            .resolve_root()
            .ok_or_else(|| SystemError::BadRequest("New API address is not configured".into()))?;
        let state = pkce::random_token();
        let verifier = pkce::random_token();
        let authorize_url = pkce::authorize_url(
            &root,
            &pkce::redirect_uri(port),
            &state,
            &pkce::code_challenge(&verifier),
            &pkce::device_name(),
        )
        .map_err(|_| SystemError::BadRequest("New API address is not a valid URL".into()))?;
        *self.pending.lock().expect("pending lock poisoned") = Some(PendingLogin {
            state,
            verifier,
            root,
            port,
            created: Instant::now(),
        });
        Ok(NewApiLoginStartResponse { authorize_url })
    }

    /// `Ok` = login completed. `Err` = message for the result page and the event.
    pub async fn complete_login(
        &self,
        user_id: &str,
        code: Option<&str>,
        state: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        let pending = {
            let mut slot = self.pending.lock().expect("pending lock poisoned");
            match slot.as_ref() {
                None => return Err(MSG_EXPIRED.to_owned()),
                Some(p) if p.created.elapsed() > PENDING_TTL => {
                    *slot = None;
                    return Err(MSG_EXPIRED.to_owned());
                }
                // A mismatched state must not break a login that is in progress.
                Some(p) if state != Some(p.state.as_str()) => return Err(MSG_EXPIRED.to_owned()),
                Some(_) => {}
            }
            slot.take().expect("pending checked above")
        };

        let result = self.finish_login(user_id, pending, code, error).await;
        if let Err(message) = &result {
            tracing::warn!(reason = %message, "newapi login failed");
        }
        let event = match &result {
            Ok(()) => NewApiAuthChangedEvent {
                state: NewApiAuthState::Ready,
                error: None,
            },
            Err(message) => NewApiAuthChangedEvent {
                state: NewApiAuthState::NeedsLogin,
                error: Some(message.clone()),
            },
        };
        let payload = serde_json::to_value(event).expect("newapi auth event should serialize");
        self.broadcaster
            .broadcast(WebSocketMessage::new(EVENT_AUTH_CHANGED, payload));
        result
    }

    async fn finish_login(
        &self,
        user_id: &str,
        pending: PendingLogin,
        code: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        if let Some(error) = error {
            return Err(if error == "access_denied" {
                "Sign-in was cancelled.".to_owned()
            } else {
                format!("Sign-in failed: {error}")
            });
        }
        let code = code
            .filter(|c| !c.is_empty())
            .ok_or_else(|| "Sign-in failed: missing code".to_owned())?;
        let grant = self
            .client
            .exchange_code(
                &pending.root,
                code,
                &pending.verifier,
                &pkce::redirect_uri(pending.port),
            )
            .await?;
        let username = self.client.fetch_username(&pending.root, &grant.access_token).await;
        let probed = match self.client.probe_models(&pending.root, &grant.api_key).await {
            ProbeOutcome::Models(ids) => Some(ids),
            _ => None,
        };
        let existing = self.find_managed(user_id).await.map_err(|e| save_failed(&e))?;
        // When the probe fails on a re-login, keep what the provider already has.
        let models = probed
            .clone()
            .or_else(|| existing.as_ref().map(|p| p.models.clone()))
            .unwrap_or_default();
        self.upsert_provider(user_id, &pending.root, &grant.api_key, models, existing.as_ref())
            .await
            .map_err(|e| save_failed(&e))?;
        tracing::info!(
            models = probed.as_ref().map_or(0, Vec::len),
            models_probed = probed.is_some(),
            has_username = username.is_some(),
            "newapi login completed"
        );
        *self.username.lock().expect("username lock poisoned") = username;
        Ok(())
    }

    async fn find_managed(&self, user_id: &str) -> Result<Option<ProviderResponse>, SystemError> {
        Ok(self
            .provider_service
            .list(user_id)
            .await?
            .into_iter()
            .find(|p| p.id == MANAGED_PROVIDER_ID))
    }

    /// Login-path write: creates or updates the managed provider with the new key, enabled.
    async fn upsert_provider(
        &self,
        user_id: &str,
        root: &str,
        api_key: &str,
        models: Vec<String>,
        existing: Option<&ProviderResponse>,
    ) -> Result<(), SystemError> {
        let base_url = config::provider_base_url(root);
        let model_settings = pinned_model_settings(&models, existing);
        match existing {
            None => {
                self.provider_service
                    .create(
                        user_id,
                        CreateProviderRequest {
                            id: Some(MANAGED_PROVIDER_ID.to_owned()),
                            platform: MANAGED_PLATFORM.to_owned(),
                            name: MANAGED_NAME.to_owned(),
                            base_url,
                            api_key: api_key.to_owned(),
                            models,
                            enabled: true,
                            capabilities: Vec::new(),
                            context_limit: None,
                            model_protocols: None,
                            model_enabled: None,
                            model_health: None,
                            model_settings,
                            bedrock_config: None,
                            is_full_url: false,
                        },
                    )
                    .await?;
            }
            Some(_) => {
                self.provider_service
                    .update(
                        user_id,
                        MANAGED_PROVIDER_ID,
                        UpdateProviderRequest {
                            api_key: Some(api_key.to_owned()),
                            base_url: Some(base_url),
                            models: Some(models),
                            model_settings: Some(model_settings),
                            enabled: Some(true),
                            ..Default::default()
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn age_pending(&self, by: Duration) {
        if let Some(p) = self.pending.lock().unwrap().as_mut() {
            p.created = Instant::now().checked_sub(by).expect("clock too close to boot");
        }
    }
}

/// Every model is pinned to chat completions; an existing `image_input` choice is kept.
fn pinned_model_settings(models: &[String], existing: Option<&ProviderResponse>) -> HashMap<String, ModelSettings> {
    models
        .iter()
        .map(|id| {
            let image_input = existing
                .and_then(|p| p.model_settings.get(id))
                .and_then(|s| s.image_input);
            (
                id.clone(),
                ModelSettings {
                    openai_api_mode: Some(ModelOpenAiApiMode::ChatCompletions),
                    image_input,
                },
            )
        })
        .collect()
}

/// Message for a failed provider write; never includes request data (it holds the key).
fn save_failed(error: &SystemError) -> String {
    tracing::warn!(%error, "newapi login: saving the managed provider failed");
    "Sign-in failed: could not save the provider".to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aionui_db::{SqliteProviderRepository, init_database_memory};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const USER: &str = "user-1";

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<WebSocketMessage<serde_json::Value>>>,
    }

    impl EventBroadcaster for Recorder {
        fn broadcast(&self, event: WebSocketMessage<serde_json::Value>) {
            self.events.lock().unwrap().push(event);
        }
    }

    struct Fixture {
        svc: NewApiAuthService,
        providers: ProviderService,
        server: MockServer,
        events: Arc<Recorder>,
        _dir: tempfile::TempDir,
        _db: aionui_db::Database,
    }

    async fn fixture() -> Fixture {
        let db = init_database_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, user_type, username, password_hash, status, session_generation, created_at, updated_at) \
             VALUES (?, 'local', ?, '', 'active', 0, 1, 1)",
        )
        .bind(USER)
        .bind(USER)
        .execute(db.pool())
        .await
        .unwrap();
        let providers = ProviderService::new(Arc::new(SqliteProviderRepository::new(db.pool().clone())), [0x42; 32]);
        let server = MockServer::start().await;
        let events = Arc::new(Recorder::default());
        let dir = tempfile::tempdir().unwrap();
        let svc = NewApiAuthService::new(
            providers.clone(),
            NewApiClient::new(reqwest::Client::new()),
            events.clone(),
            dir.path().to_path_buf(),
        )
        .with_url_sources(Some(server.uri()), None);
        Fixture {
            svc,
            providers,
            server,
            events,
            _dir: dir,
            _db: db,
        }
    }

    impl Fixture {
        async fn mock_models(&self, status: u16, ids: &[&str]) {
            let data: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
            Mock::given(method("GET"))
                .and(path("/v1/models"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({ "data": data })))
                .mount(&self.server)
                .await;
        }

        async fn mock_token(&self, api_key: &str) {
            Mock::given(method("POST"))
                .and(path("/oauth2/token"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({ "api_key": api_key, "access_token": "at-1" })),
                )
                .mount(&self.server)
                .await;
            Mock::given(method("GET"))
                .and(path("/oauth2/userinfo"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "preferred_username": "alice" })))
                .mount(&self.server)
                .await;
        }

        async fn login(&self, port: u16, code: &str) -> Result<(), String> {
            let url = self.svc.start_login(port).unwrap().authorize_url;
            self.svc
                .complete_login(USER, Some(code), Some(&state_of(&url)), None)
                .await
        }

        async fn managed(&self) -> Option<ProviderResponse> {
            self.providers
                .list(USER)
                .await
                .unwrap()
                .into_iter()
                .find(|p| p.id == MANAGED_PROVIDER_ID)
        }

        fn events(&self) -> Vec<serde_json::Value> {
            self.events
                .events
                .lock()
                .unwrap()
                .iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect()
        }

        async fn seed_provider(&self, api_key: &str, models: &[&str]) {
            self.providers
                .create(
                    USER,
                    CreateProviderRequest {
                        id: Some(MANAGED_PROVIDER_ID.into()),
                        platform: "new-api".into(),
                        name: "New API".into(),
                        base_url: format!("{}/v1", self.server.uri()),
                        api_key: api_key.into(),
                        models: models.iter().map(|m| (*m).to_owned()).collect(),
                        enabled: true,
                        capabilities: vec![],
                        context_limit: None,
                        model_protocols: None,
                        model_enabled: None,
                        model_health: None,
                        model_settings: HashMap::new(),
                        bedrock_config: None,
                        is_full_url: false,
                    },
                )
                .await
                .unwrap();
        }
    }

    fn query_param(url: &str, key: &str) -> String {
        url::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == key)
            .unwrap()
            .1
            .into_owned()
    }

    fn state_of(url: &str) -> String {
        query_param(url, "state")
    }

    #[tokio::test]
    async fn status_not_configured() {
        let mut f = fixture().await;
        f.svc = f.svc.clone().with_url_sources(None, None);
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.state, NewApiAuthState::NotConfigured);
        assert!(s.base_url.is_none());
        assert!(s.config_path.ends_with("newapi-client.json"));
    }

    #[tokio::test]
    async fn status_no_token_when_provider_missing() {
        let f = fixture().await;
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.state, NewApiAuthState::NeedsLogin);
        assert_eq!(s.reason, Some(NewApiAuthReason::NoToken));
        assert_eq!(s.base_url.as_deref(), Some(f.server.uri().as_str()));
    }

    #[tokio::test]
    async fn status_no_token_when_key_blank() {
        let f = fixture().await;
        f.seed_provider("sk-a", &[]).await;
        // Blank keys cannot be created through validation, so blank it via update.
        f.providers
            .update(
                USER,
                MANAGED_PROVIDER_ID,
                UpdateProviderRequest {
                    api_key: Some("   ".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.reason, Some(NewApiAuthReason::NoToken));
    }

    #[tokio::test]
    async fn status_token_rejected_on_401() {
        let f = fixture().await;
        f.seed_provider("sk-a", &[]).await;
        f.mock_models(401, &[]).await;
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.state, NewApiAuthState::NeedsLogin);
        assert_eq!(s.reason, Some(NewApiAuthReason::TokenRejected));
    }

    #[tokio::test]
    async fn status_offline_keeps_models() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["m1"]).await;
        f.mock_models(500, &[]).await;
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.state, NewApiAuthState::Ready);
        assert_eq!(s.reason, Some(NewApiAuthReason::Offline));
        assert_eq!(f.managed().await.unwrap().models, vec!["m1".to_owned()]);
    }

    #[tokio::test]
    async fn status_offline_on_html_body() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["m1"]).await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>gateway</html>"))
            .mount(&f.server)
            .await;
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.state, NewApiAuthState::Ready);
        assert_eq!(s.reason, Some(NewApiAuthReason::Offline));
    }

    #[tokio::test]
    async fn status_ready_refreshes_models_and_pins_chat_completions() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["old"]).await;
        f.mock_models(200, &["gpt-5.6", "claude-sonnet-4"]).await;
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.state, NewApiAuthState::Ready);
        assert_eq!(s.reason, None);
        let p = f.managed().await.unwrap();
        assert_eq!(p.models, vec!["gpt-5.6".to_owned(), "claude-sonnet-4".to_owned()]);
        for id in ["gpt-5.6", "claude-sonnet-4"] {
            assert_eq!(
                p.model_settings[id].openai_api_mode,
                Some(ModelOpenAiApiMode::ChatCompletions)
            );
        }
        assert!(p.model_protocols.is_none());
    }

    #[tokio::test]
    async fn status_preserves_model_enabled() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["gpt-4o"]).await;
        f.providers
            .update(
                USER,
                MANAGED_PROVIDER_ID,
                UpdateProviderRequest {
                    model_enabled: Some(HashMap::from([("gpt-4o".to_owned(), false)])),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        f.mock_models(200, &["gpt-4o", "gpt-5"]).await;
        f.svc.status(USER).await.unwrap();
        let p = f.managed().await.unwrap();
        assert_eq!(p.model_enabled, Some(HashMap::from([("gpt-4o".to_owned(), false)])));
    }

    #[tokio::test]
    async fn status_after_provider_deleted_is_no_token() {
        let f = fixture().await;
        f.mock_token("sk-xyz").await;
        f.mock_models(200, &["m1"]).await;
        f.login(53124, "c1").await.unwrap();
        f.providers.delete(USER, MANAGED_PROVIDER_ID).await.unwrap();
        let s = f.svc.status(USER).await.unwrap();
        assert_eq!(s.reason, Some(NewApiAuthReason::NoToken));
    }

    #[tokio::test]
    async fn status_updates_base_url_when_config_changed() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["m1"]).await;
        f.providers
            .update(
                USER,
                MANAGED_PROVIDER_ID,
                UpdateProviderRequest {
                    base_url: Some("https://old.example.com/v1".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        f.mock_models(200, &["m1"]).await;
        f.svc.status(USER).await.unwrap();
        assert_eq!(f.managed().await.unwrap().base_url, format!("{}/v1", f.server.uri()));
    }

    #[tokio::test]
    async fn status_keeps_image_input_setting() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["m1"]).await;
        f.providers
            .update(
                USER,
                MANAGED_PROVIDER_ID,
                UpdateProviderRequest {
                    model_settings: Some(HashMap::from([(
                        "m1".to_owned(),
                        ModelSettings {
                            image_input: Some(aionui_api_types::ModelImageInputCapability::Supported),
                            openai_api_mode: None,
                        },
                    )])),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        f.mock_models(200, &["m1"]).await;
        f.svc.status(USER).await.unwrap();
        let s = f.managed().await.unwrap().model_settings["m1"].clone();
        assert_eq!(
            s.image_input,
            Some(aionui_api_types::ModelImageInputCapability::Supported)
        );
        assert_eq!(s.openai_api_mode, Some(ModelOpenAiApiMode::ChatCompletions));
    }

    async fn mock_models_delayed(f: &Fixture, ids: &[&str]) {
        let data: Vec<_> = ids.iter().map(|id| json!({ "id": id })).collect();
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "data": data }))
                    .set_delay(Duration::from_millis(400)),
            )
            .mount(&f.server)
            .await;
    }

    #[tokio::test]
    async fn status_does_not_revert_key_replaced_during_probe() {
        let f = fixture().await;
        f.seed_provider("sk-old", &["old"]).await;
        mock_models_delayed(&f, &["m-old"]).await;
        let status = f.svc.status(USER);
        let replace = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            f.providers
                .update(
                    USER,
                    MANAGED_PROVIDER_ID,
                    UpdateProviderRequest {
                        api_key: Some("sk-new".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        };
        let (s, ()) = tokio::join!(status, replace);
        assert_eq!(s.unwrap().state, NewApiAuthState::Ready);
        let p = f.managed().await.unwrap();
        assert_eq!(p.api_key, "sk-new");
        assert_eq!(p.models, vec!["old".to_owned()]);
    }

    #[tokio::test]
    async fn status_does_not_reenable_disabled_provider() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["old"]).await;
        f.providers
            .update(
                USER,
                MANAGED_PROVIDER_ID,
                UpdateProviderRequest {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        f.mock_models(200, &["new"]).await;
        f.svc.status(USER).await.unwrap();
        let p = f.managed().await.unwrap();
        assert_eq!(p.models, vec!["new".to_owned()]);
        assert!(!p.enabled);
    }

    #[tokio::test]
    async fn status_no_token_when_provider_deleted_during_probe() {
        let f = fixture().await;
        f.seed_provider("sk-a", &["old"]).await;
        mock_models_delayed(&f, &["new"]).await;
        let status = f.svc.status(USER);
        let delete = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            f.providers.delete(USER, MANAGED_PROVIDER_ID).await.unwrap();
        };
        let (s, ()) = tokio::join!(status, delete);
        let s = s.unwrap();
        assert_eq!(s.state, NewApiAuthState::NeedsLogin);
        assert_eq!(s.reason, Some(NewApiAuthReason::NoToken));
    }

    #[tokio::test]
    async fn complete_login_creates_managed_provider() {
        let f = fixture().await;
        f.mock_token("sk-xyz").await;
        f.mock_models(200, &["m1"]).await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        f.svc
            .complete_login(USER, Some("code-1"), Some(&state_of(&url)), None)
            .await
            .unwrap();

        let p = f.managed().await.unwrap();
        assert_eq!(p.platform, "new-api");
        assert_eq!(p.name, "New API");
        assert_eq!(p.base_url, format!("{}/v1", f.server.uri()));
        assert_eq!(p.api_key, "sk-xyz");
        assert_eq!(p.models, vec!["m1".to_owned()]);
        assert!(p.enabled);
        assert_eq!(f.events()[0]["name"], "newapi.authChanged");
        assert_eq!(f.events()[0]["data"]["state"], "ready");
        assert_eq!(f.events().len(), 1);

        let reqs = f.server.received_requests().await.unwrap();
        let token_req = reqs.iter().find(|r| r.url.path() == "/oauth2/token").unwrap();
        let form: HashMap<String, String> = url::form_urlencoded::parse(&token_req.body).into_owned().collect();
        let digest = Sha256::digest(form["code_verifier"].as_bytes());
        let expected = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest);
        assert_eq!(expected, query_param(&url, "code_challenge"));
        assert_eq!(form["redirect_uri"], "http://127.0.0.1:53124/api/newapi/login/callback");
        assert_eq!(f.svc.status(USER).await.unwrap().username.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn complete_login_updates_existing_key() {
        let f = fixture().await;
        f.mock_token("sk-one").await;
        f.mock_models(200, &["m1"]).await;
        f.login(53124, "c1").await.unwrap();
        f.server.reset().await;
        f.mock_token("sk-two").await;
        f.mock_models(200, &["m1"]).await;
        f.login(53124, "c2").await.unwrap();
        let all = f.providers.list(USER).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].api_key, "sk-two");
    }

    #[tokio::test]
    async fn complete_login_wrong_state_keeps_pending() {
        let f = fixture().await;
        f.mock_token("sk-xyz").await;
        f.mock_models(200, &["m1"]).await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        let err = f
            .svc
            .complete_login(USER, Some("c"), Some("bogus"), None)
            .await
            .unwrap_err();
        assert_eq!(err, MSG_EXPIRED);
        assert!(f.events().is_empty());
        f.svc
            .complete_login(USER, Some("c"), Some(&state_of(&url)), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn complete_login_is_single_use() {
        let f = fixture().await;
        f.mock_token("sk-xyz").await;
        f.mock_models(200, &["m1"]).await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        let state = state_of(&url);
        f.svc.complete_login(USER, Some("c"), Some(&state), None).await.unwrap();
        let err = f
            .svc
            .complete_login(USER, Some("c"), Some(&state), None)
            .await
            .unwrap_err();
        assert_eq!(err, MSG_EXPIRED);
        assert_eq!(f.managed().await.unwrap().api_key, "sk-xyz");
        assert_eq!(f.events().len(), 1);
    }

    #[tokio::test]
    async fn complete_login_expired_pending() {
        let f = fixture().await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        f.svc.age_pending(PENDING_TTL + Duration::from_secs(1));
        let err = f
            .svc
            .complete_login(USER, Some("c"), Some(&state_of(&url)), None)
            .await
            .unwrap_err();
        assert_eq!(err, MSG_EXPIRED);
        assert!(f.events().is_empty());
    }

    #[tokio::test]
    async fn second_start_invalidates_first() {
        let f = fixture().await;
        let first = state_of(&f.svc.start_login(53124).unwrap().authorize_url);
        let second = state_of(&f.svc.start_login(53124).unwrap().authorize_url);
        f.mock_token("sk-xyz").await;
        f.mock_models(200, &["m1"]).await;
        let err = f
            .svc
            .complete_login(USER, Some("c"), Some(&first), None)
            .await
            .unwrap_err();
        assert_eq!(err, MSG_EXPIRED);
        f.svc
            .complete_login(USER, Some("c"), Some(&second), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn complete_login_other_error_is_reported() {
        let f = fixture().await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        let err = f
            .svc
            .complete_login(USER, None, Some(&state_of(&url)), Some("server_error"))
            .await
            .unwrap_err();
        assert_eq!(err, "Sign-in failed: server_error");
        assert_eq!(f.events()[0]["data"]["error"], "Sign-in failed: server_error");
    }

    #[tokio::test]
    async fn complete_login_missing_code() {
        let f = fixture().await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        let err = f
            .svc
            .complete_login(USER, None, Some(&state_of(&url)), None)
            .await
            .unwrap_err();
        assert_eq!(err, "Sign-in failed: missing code");
    }

    #[tokio::test]
    async fn complete_login_models_unavailable_keeps_existing_models() {
        let f = fixture().await;
        f.seed_provider("sk-old", &["keep-me"]).await;
        f.mock_token("sk-new").await;
        f.mock_models(500, &[]).await;
        f.login(53124, "c").await.unwrap();
        let p = f.managed().await.unwrap();
        assert_eq!(p.api_key, "sk-new");
        assert_eq!(p.models, vec!["keep-me".to_owned()]);
        assert_eq!(
            p.model_settings["keep-me"].openai_api_mode,
            Some(ModelOpenAiApiMode::ChatCompletions)
        );
    }

    #[tokio::test]
    async fn complete_login_access_denied() {
        let f = fixture().await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        let err = f
            .svc
            .complete_login(USER, None, Some(&state_of(&url)), Some("access_denied"))
            .await
            .unwrap_err();
        assert_eq!(err, "Sign-in was cancelled.");
        let events = f.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["data"]["state"], "needs_login");
    }

    #[tokio::test]
    async fn complete_login_exchange_failure_reports_description() {
        let f = fixture().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({ "error": "invalid_grant", "error_description": "code expired" })),
            )
            .mount(&f.server)
            .await;
        let url = f.svc.start_login(53124).unwrap().authorize_url;
        let err = f
            .svc
            .complete_login(USER, Some("c"), Some(&state_of(&url)), None)
            .await
            .unwrap_err();
        assert_eq!(err, "code expired");
        assert_eq!(f.events()[0]["data"]["error"], "code expired");
        assert!(f.managed().await.is_none());
    }

    #[tokio::test]
    async fn complete_login_models_unavailable_still_saves_key() {
        let f = fixture().await;
        f.mock_token("sk-xyz").await;
        f.mock_models(500, &[]).await;
        f.login(53124, "c").await.unwrap();
        let p = f.managed().await.unwrap();
        assert_eq!(p.api_key, "sk-xyz");
        assert!(p.models.is_empty());
    }

    #[tokio::test]
    async fn start_login_without_base_url_is_bad_request() {
        let mut f = fixture().await;
        f.svc = f.svc.clone().with_url_sources(None, None);
        assert!(matches!(f.svc.start_login(53124), Err(SystemError::BadRequest(_))));
    }
}
