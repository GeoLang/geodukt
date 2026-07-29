//! JWT authentication for the pipeline run endpoint.
//!
//! Claims are `sub`/`exp`/`role` (HS256) signed with the shared platform
//! secret, the same shape ptolemy and tiletopia validate, so one token works
//! across services.
//!
//! Only `POST /run` is gated: it reads and writes files the manifest names, so
//! it needs an identity to record and a role to check. `/validate`,
//! `/operations` and `/health` have no side effects and stay open, because
//! headless planning and the eval harness call them without a token.

use axum::Json;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::{DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};

/// Env var holding the shared HS256 secret. Unset means dev mode: no gate.
pub const SECRET_ENV: &str = "PLATFORM_JWT_SECRET";

/// JWT claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    pub role: String,
}

impl Claims {
    /// Unknown role strings grant nothing, so a typo cannot open a run.
    pub fn can_run(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "editor")
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

    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty());

    let Some(token) = token else {
        return deny(StatusCode::UNAUTHORIZED, "missing bearer token");
    };

    // the decode error is not echoed back: it separates "expired" from "bad
    // signature", which helps an attacker more than a caller
    let key = DecodingKey::from_secret(secret.as_bytes());
    let Ok(data) = decode::<Claims>(token, &key, &Validation::default()) else {
        return deny(StatusCode::UNAUTHORIZED, "invalid or expired token");
    };

    if !data.claims.can_run() {
        return deny(StatusCode::FORBIDDEN, "editor or admin role required");
    }

    request.extensions_mut().insert(data.claims);
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
            role: role.into(),
        };
        assert!(claims("admin").can_run());
        assert!(claims("editor").can_run());
        assert!(!claims("viewer").can_run());
        // wrong case is not a known role
        assert!(!claims("Editor").can_run());
        assert!(!claims("").can_run());
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
