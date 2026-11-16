// fork(newapi): wire-format DTOs for the managed New API login — see docs/fork/newapi-overlay.md
use serde::{Deserialize, Serialize};

/// Overall state of the managed New API login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewApiAuthState {
    Ready,
    NeedsLogin,
    NotConfigured,
}

/// Why the login is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewApiAuthReason {
    NoToken,
    TokenRejected,
    Offline,
}

/// Response of `GET /api/newapi/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewApiStatusResponse {
    pub state: NewApiAuthState,
    /// Root address of the New API server (without `/v1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<NewApiAuthReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Absolute path of the client config file (shown to the user as a hint).
    pub config_path: String,
}

/// Response of `POST /api/newapi/login/start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewApiLoginStartResponse {
    pub authorize_url: String,
}

/// Payload of the `newapi.authChanged` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewApiAuthChangedEvent {
    pub state: NewApiAuthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(reason: Option<NewApiAuthReason>) -> NewApiStatusResponse {
        NewApiStatusResponse {
            state: NewApiAuthState::Ready,
            base_url: None,
            reason,
            username: None,
            config_path: "/x/newapi-client.json".to_owned(),
        }
    }

    #[test]
    fn status_omits_none_fields() {
        let json = serde_json::to_value(status(None)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"state":"ready","config_path":"/x/newapi-client.json"})
        );
    }

    #[test]
    fn status_serializes_reason_snake_case() {
        let json = serde_json::to_value(status(Some(NewApiAuthReason::TokenRejected))).unwrap();
        assert_eq!(json["reason"], "token_rejected");
    }

    #[test]
    fn changed_event_omits_none_error() {
        let ev = NewApiAuthChangedEvent {
            state: NewApiAuthState::NeedsLogin,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(ev).unwrap(),
            serde_json::json!({"state":"needs_login"})
        );
    }
}
