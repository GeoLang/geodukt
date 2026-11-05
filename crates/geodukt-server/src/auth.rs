//! JWT authentication for the pipeline run endpoint.
//!
//! Claims are `sub`/`exp`/`role` (HS256) signed with the shared platform
//! secret, the same shape ptolemy and tiletopia validate, so one token works
//! across services.
//!
//! `POST /run` and the `/gp` tools accept a normal user token with the editor
//! or admin role, or a role-free tool token carrying the exact `geodukt:run`
//! scope.
//! The run history, `GET /runs` and `GET /runs/{id}`, needs a valid token of any
//! role, and the subject each record carries decides which of them come back.
//! Tool tokens cannot read history.
//! `/validate`, `/operations` and `/health` have no side effects and stay open,
//! because headless planning and the eval harness call them without a token.

use axum::Json;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{StatusCode, request::Parts};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use platform_auth::{TokenAuthority, VerifiedToken, bearer_token, verify_token};

/// Env var holding the shared HS256 secret. Unset means dev mode: no gate.
pub const SECRET_ENV: &str = "PLATFORM_JWT_SECRET";
pub const GEODUKT_RUN_SCOPE: &str = "geodukt:run";

fn can_run(token: &VerifiedToken) -> bool {
    match &token.authority {
        TokenAuthority::Tool { .. } => token.allows_scope(GEODUKT_RUN_SCOPE),
        TokenAuthority::Platform { role } => matches!(role.as_deref(), Some("admin" | "editor")),
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
    pub fn from_env() -> std::io::Result<Self> {
        let secret = std::env::var_os(SECRET_ENV)
            .map(|secret| {
                secret.into_string().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{SECRET_ENV} is set but not UTF-8"),
                    )
                })
            })
            .transpose()?;
        Ok(Self::new(secret))
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

fn verify(request: &Request, secret: &str) -> Result<VerifiedToken, (StatusCode, &'static str)> {
    let Some(token) = bearer_token(request.headers()) else {
        return Err((StatusCode::UNAUTHORIZED, "missing bearer token"));
    };

    // verify_token accepts platform tokens with no role
    verify_token(token, secret)
        .ok()
        .filter(|verified| !matches!(verified.authority, TokenAuthority::Platform { role: None }))
        .ok_or((StatusCode::UNAUTHORIZED, "invalid or expired token"))
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

    let verified = match verify(&request, secret) {
        Ok(verified) => verified,
        Err((status, message)) => return deny(status, message),
    };

    if !can_run(&verified) {
        let message = match verified.authority {
            TokenAuthority::Tool { .. } => "geodukt:run scope required",
            TokenAuthority::Platform { .. } => "editor or admin role required",
        };
        return deny(StatusCode::FORBIDDEN, message);
    }

    request.extensions_mut().insert(verified);
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

    let verified = match verify(&request, secret) {
        Ok(verified) => verified,
        Err((status, message)) => return deny(status, message),
    };

    if matches!(verified.authority, TokenAuthority::Tool { .. }) {
        return deny(StatusCode::FORBIDDEN, "platform user token required");
    }

    request.extensions_mut().insert(verified);
    next.run(request).await
}

/// The verified caller, absent in dev mode.
#[derive(Debug, Clone)]
pub struct Caller(Option<VerifiedToken>);

impl Caller {
    /// Subject to record on the run, or `None` when nothing was verified.
    pub fn sub(&self) -> Option<String> {
        self.0.as_ref().map(|verified| verified.sub.clone())
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
        let verified = caller.0.as_ref()?;
        let is_admin = matches!(
            &verified.authority,
            TokenAuthority::Platform { role } if role.as_deref() == Some("admin")
        );
        if is_admin {
            return Some(RunVisibility::All);
        }
        Some(RunVisibility::Own(verified.sub.clone()))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Caller(parts.extensions.get::<VerifiedToken>().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform_token(role: &str) -> VerifiedToken {
        VerifiedToken {
            sub: "u1".into(),
            exp: 0,
            name: None,
            authority: TokenAuthority::Platform {
                role: Some(role.into()),
            },
        }
    }

    #[test]
    fn only_editor_and_admin_can_run() {
        assert!(can_run(&platform_token("admin")));
        assert!(can_run(&platform_token("editor")));
        assert!(!can_run(&platform_token("viewer")));
        // wrong case is not a known role
        assert!(!can_run(&platform_token("Editor")));
        assert!(!can_run(&platform_token("")));
    }

    #[test]
    fn only_admin_reads_other_callers_runs() {
        let config = AuthConfig::new(Some("0123456789abcdef".into()));
        let visibility = |role: &str| config.run_visibility(&Caller(Some(platform_token(role))));
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
