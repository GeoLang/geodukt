//! JWT authentication for the pipeline run endpoint.
//!
//! Claims are `sub`/`exp`/`role` (HS256) signed with the shared platform
//! secret, the same shape ptolemy and tiletopia validate, so one token works
//! across services.
//!
//! `POST /run` accepts a normal user token with the editor or admin role, or a
//! role-free tool token carrying the exact `geodukt:run` scope.
//! The run history, `GET /runs` and `GET /runs/{id}`, needs a valid token of any
//! role, and the subject each record carries decides which of them come back.
//! Tool tokens cannot read history.
//! `/validate`, `/operations` and `/health` have no side effects and stay open,
//! because headless planning and the eval harness call them without a token.

use axum::Json;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::{DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};

/// Env var holding the shared HS256 secret. Unset means dev mode: no gate.
pub const SECRET_ENV: &str = "PLATFORM_JWT_SECRET";
pub const TOOL_TOKEN_USE: &str = "tool";
pub const GEODUKT_RUN_SCOPE: &str = "geodukt:run";

/// JWT claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_use: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<String>>,
}

impl Claims {
    fn valid_contract(&self) -> bool {
        match self.token_use.as_deref() {
            None => self.role.is_some(),
            Some(TOOL_TOKEN_USE) => self.role.is_none() && self.scope.is_some(),
            Some(_) => false,
        }
    }

    fn is_tool_token(&self) -> bool {
        self.token_use.as_deref() == Some(TOOL_TOKEN_USE)
    }

    fn has_scope(&self, required: &str) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|scopes| scopes.iter().any(|scope| scope == required))
    }

    /// A user role or the exact scoped operation may start a run.
    pub fn can_run(&self) -> bool {
        if self.is_tool_token() {
            return self.has_scope(GEODUKT_RUN_SCOPE);
        }
        matches!(self.role.as_deref(), Some("admin" | "editor"))
    }

    /// The instance-wide administrator, the role that reads other callers'
    /// runs. Same string ptolemy, tiletopia and collecta admit.
    pub fn can_admin(&self) -> bool {
        !self.is_tool_token() && self.role.as_deref() == Some("admin")
    }
}

/// Signing secret, or nothing when the service runs unauthenticated.
#[derive(Clone)]
pub struct AuthConfig {
    secret: Option<String>,
}

/// Redacted so a stray `{:?}` cannot put the secret in a log line.
impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("enabled", &self.enabled())
            .finish()
    }
}

impl AuthConfig {
    /// A missing or empty secret means auth off, matching the other services'
    /// dev mode.
    pub fn from_env() -> Self {
        Self::new(std::env::var(SECRET_ENV).ok())
    }

    pub fn new(secret: Option<String>) -> Self {
        Self {
            secret: secret.filter(|s| !s.is_empty()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.secret.is_some()
    }
}

fn deny(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}

/// The bearer token's claims, or the status and message to deny with.
fn verify(request: &Request, secret: &str) -> Result<Claims, (StatusCode, &'static str)> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty());

    let Some(token) = token else {
        return Err((StatusCode::UNAUTHORIZED, "missing bearer token"));
    };

    // the decode error is not echoed back: it separates "expired" from "bad
    // signature", which helps an attacker more than a caller
    let key = DecodingKey::from_secret(secret.as_bytes());
    let Ok(data) = decode::<Claims>(token, &key, &Validation::default()) else {
        return Err((StatusCode::UNAUTHORIZED, "invalid or expired token"));
    };
    if !data.claims.valid_contract() {
        return Err((StatusCode::UNAUTHORIZED, "invalid or expired token"));
    }

    Ok(data.claims)
}

/// Require a valid token with role `editor` or `admin`. Passes everything
/// through when no secret is configured.
pub async fn require_run_access(
    State(config): State<AuthConfig>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(secret) = config.secret.as_deref() else {
        return next.run(request).await;
    };

    let claims = match verify(&request, secret) {
        Ok(claims) => claims,
        Err((status, message)) => return deny(status, message),
    };

    if !claims.can_run() {
        if claims.is_tool_token() {
            return deny(StatusCode::FORBIDDEN, "geodukt:run scope required");
        }
        return deny(StatusCode::FORBIDDEN, "editor or admin role required");
    }

    request.extensions_mut().insert(claims);
    next.run(request).await
}

/// Require a valid token, any role. The run history names callers, so reading it
/// needs an identity, and [`AuthConfig::run_visibility`] narrows that identity to
/// the records it may see. Passes everything through when no secret is
/// configured.
pub async fn require_history_access(
    State(config): State<AuthConfig>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(secret) = config.secret.as_deref() else {
        return next.run(request).await;
    };

    let claims = match verify(&request, secret) {
        Ok(claims) => claims,
        Err((status, message)) => return deny(status, message),
    };

    if claims.is_tool_token() {
        return deny(StatusCode::FORBIDDEN, "platform user token required");
    }

    request.extensions_mut().insert(claims);
    next.run(request).await
}

/// The verified caller, absent in dev mode.
#[derive(Debug, Clone)]
pub struct Caller(Option<Claims>);

impl Caller {
    /// Subject to record on the run, or `None` when nothing was verified.
    pub fn sub(&self) -> Option<String> {
        self.0.as_ref().map(|c| c.sub.clone())
    }
}

/// Which recorded runs a caller may read.
#[derive(Debug, Clone, PartialEq)]
pub enum RunVisibility {
    All,
    /// Only runs recorded for this subject.
    Own(String),
}

impl RunVisibility {
    /// The subject a run must carry to be visible, or `None` when every run is.
    /// A run recorded with no subject is read only by a caller reading all of
    /// them.
    pub fn required_subject(&self) -> Option<&str> {
        match self {
            RunVisibility::All => None,
            RunVisibility::Own(sub) => Some(sub),
        }
    }
}

impl AuthConfig {
    /// What of the run history this caller reads. The whole history only with
    /// the gate off, where no run carries a subject to filter by, or for an
    /// instance admin: every other role, known or not, reads its own runs.
    /// `None` means the gate is on and nothing was verified, which is a request
    /// that skipped [`require_history_access`].
    pub fn run_visibility(&self, caller: &Caller) -> Option<RunVisibility> {
        if !self.enabled() {
            return Some(RunVisibility::All);
        }
        let claims = caller.0.as_ref()?;
        if claims.can_admin() {
            return Some(RunVisibility::All);
        }
        Some(RunVisibility::Own(claims.sub.clone()))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Caller(parts.extensions.get::<Claims>().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_editor_and_admin_can_run() {
        let claims = |role: &str| Claims {
            sub: "u1".into(),
            exp: 0,
            role: Some(role.into()),
            token_use: None,
            scope: None,
        };
        assert!(claims("admin").can_run());
        assert!(claims("editor").can_run());
        assert!(!claims("viewer").can_run());
        // wrong case is not a known role
        assert!(!claims("Editor").can_run());
        assert!(!claims("").can_run());
    }

    #[test]
    fn only_admin_reads_other_callers_runs() {
        let config = AuthConfig::new(Some("0123456789abcdef".into()));
        let visibility = |role: &str| {
            config.run_visibility(&Caller(Some(Claims {
                sub: "u1".into(),
                exp: 0,
                role: Some(role.into()),
                token_use: None,
                scope: None,
            })))
        };
        assert_eq!(visibility("admin"), Some(RunVisibility::All));
        assert_eq!(
            visibility("editor"),
            Some(RunVisibility::Own("u1".to_string()))
        );
        // what the history query filters on
        assert_eq!(visibility("admin").unwrap().required_subject(), None);
        assert_eq!(visibility("editor").unwrap().required_subject(), Some("u1"));
        assert_eq!(
            visibility("wizard"),
            Some(RunVisibility::Own("u1".to_string()))
        );
        // gate on, nothing verified: no run is visible
        assert_eq!(config.run_visibility(&Caller(None)), None);
        // gate off: no subject is recorded to filter by
        assert_eq!(
            AuthConfig::new(None).run_visibility(&Caller(None)),
            Some(RunVisibility::All)
        );
    }

    #[test]
    fn empty_secret_is_dev_mode() {
        assert!(!AuthConfig::new(None).enabled());
        assert!(!AuthConfig::new(Some(String::new())).enabled());
        assert!(AuthConfig::new(Some("0123456789abcdef".into())).enabled());
    }

    #[test]
    fn debug_does_not_print_the_secret() {
        let rendered = format!("{:?}", AuthConfig::new(Some("s3cr3t-value".into())));
        assert!(!rendered.contains("s3cr3t-value"), "{rendered}");
    }
}
