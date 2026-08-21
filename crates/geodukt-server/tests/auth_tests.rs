//! Tests for the JWT gate on POST /run, the /gp tools, and the run history.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use geodukt_server::auth::{AuthConfig, Claims};
use geodukt_server::{create_router, create_router_with_auth};
use tower::ServiceExt;

const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn token_for(sub: &str, role: &str, ttl_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &Claims {
            sub: sub.into(),
            exp: (now + ttl_secs) as usize,
            role: Some(role.into()),
            token_use: None,
            scope: None,
        },
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}

fn token(role: &str, ttl_secs: i64) -> String {
    token_for("user-42", role, ttl_secs)
}

fn expiry(ttl_secs: i64) -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + ttl_secs
}

fn claims_token(claims: serde_json::Value) -> String {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}

fn tool_token(scopes: &[&str], ttl_secs: i64) -> String {
    claims_token(serde_json::json!({
        "sub": "user-42",
        "exp": expiry(ttl_secs),
        "token_use": "tool",
        "scope": scopes,
    }))
}

/// Runs a point through to a geojson sink, so a permitted run completes.
fn working_manifest(dir: &std::path::Path) -> String {
    let input = dir.join("in.geojson");
    std::fs::write(
        &input,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{},
             "geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
    )
    .unwrap();

    format!(
        r#"
[project]
name = "gated"

[[source]]
name = "pts"
format = "geojson"
path = "{input}"

[[sink]]
name = "out"
input = "pts"
format = "geojson"
path = "{output}"
"#,
        input = input.display(),
        output = dir.join("out.geojson").display()
    )
}

fn run_request(manifest: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    builder
        .body(Body::from(
            serde_json::json!({"manifest": manifest}).to_string(),
        ))
        .unwrap()
}

fn get_request(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    builder.body(Body::empty()).unwrap()
}

fn gp_buffer_request(bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/gp/buffer")
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    builder
        .body(Body::from(
            serde_json::json!({
                "input": {
                    "type": "FeatureCollection",
                    "features": [{
                        "type": "Feature",
                        "properties": {},
                        "geometry": {"type": "Point", "coordinates": [0.0, 0.0]}
                    }]
                },
                "params": {"distance": 1.0}
            })
            .to_string(),
        ))
        .unwrap()
}

async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn gated() -> axum::Router {
    create_router_with_auth(AuthConfig::new(Some(SECRET.into())))
}

/// A gated router holding run 0 by `user-a` and run 1 by `user-b`.
async fn history_of_two_users(dir: &std::path::Path) -> axum::Router {
    let app = gated();
    for sub in ["user-a", "user-b"] {
        let user_dir = dir.join(sub);
        std::fs::create_dir(&user_dir).unwrap();
        let manifest = working_manifest(&user_dir);
        let (status, _) = send(
            app.clone(),
            run_request(&manifest, Some(&token_for(sub, "editor", 60))),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    app
}

fn subjects(runs: &serde_json::Value) -> Vec<String> {
    runs.as_array()
        .unwrap_or_else(|| panic!("expected a run list, got {runs}"))
        .iter()
        .map(|run| run["sub"].as_str().unwrap_or("<none>").to_string())
        .collect()
}

#[tokio::test]
async fn valid_editor_token_runs_and_records_the_subject() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, record) = send(gated(), run_request(&manifest, Some(&token("editor", 60)))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(record["status"], "Completed");
    assert_eq!(record["sub"], "user-42");

    let started = parse_rfc3339(&record, "started_at");
    let finished = parse_rfc3339(&record, "finished_at");
    assert!(started <= finished, "{record}");
}

fn parse_rfc3339(record: &serde_json::Value, field: &str) -> chrono::DateTime<chrono::FixedOffset> {
    let stamp = record[field]
        .as_str()
        .unwrap_or_else(|| panic!("expected {field} on {record}"));
    chrono::DateTime::parse_from_rfc3339(stamp).unwrap()
}

#[tokio::test]
async fn admin_token_is_allowed_too() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, _) = send(gated(), run_request(&manifest, Some(&token("admin", 60)))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn scoped_tool_token_runs_and_records_the_subject() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let bearer = tool_token(&["geodukt:run"], 60);
    let (status, record) = send(gated(), run_request(&manifest, Some(&bearer))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(record["status"], "Completed");
    assert_eq!(record["sub"], "user-42");
}

#[tokio::test]
async fn empty_scope_tool_token_cannot_fall_back_to_a_role() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let empty = tool_token(&[], 60);
    let (status, body) = send(gated(), run_request(&manifest, Some(&empty))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "geodukt:run scope required");

    let role_bearing = claims_token(serde_json::json!({
        "sub": "user-42",
        "exp": expiry(60),
        "role": "admin",
        "token_use": "tool",
        "scope": [],
    }));
    let (status, _) = send(gated(), run_request(&manifest, Some(&role_bearing))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!dir.path().join("out.geojson").exists());
}

#[tokio::test]
async fn wrong_scope_is_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());
    let bearer = tool_token(&["ptolemy:read"], 60);

    let (status, body) = send(gated(), run_request(&manifest, Some(&bearer))).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "geodukt:run scope required");
    assert!(!dir.path().join("out.geojson").exists());
}

#[tokio::test]
async fn malformed_tool_claims_are_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());
    let claims = [
        serde_json::json!({
            "sub": "user-42",
            "exp": expiry(60),
            "token_use": "tool",
        }),
        serde_json::json!({
            "sub": "user-42",
            "exp": expiry(60),
            "token_use": "tool",
            "scope": "geodukt:run",
        }),
        serde_json::json!({
            "sub": "user-42",
            "exp": expiry(60),
            "token_use": "other",
            "scope": ["geodukt:run"],
        }),
    ];

    for claims in claims {
        let bearer = claims_token(claims);
        let (status, _) = send(gated(), run_request(&manifest, Some(&bearer))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert!(!dir.path().join("out.geojson").exists());
}

#[tokio::test]
async fn missing_token_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, _) = send(gated(), run_request(&manifest, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        !dir.path().join("out.geojson").exists(),
        "run must not start"
    );
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, body) = send(
        gated(),
        run_request(&manifest, Some(&token("editor", -3600))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // the reason stays vague: expired and bad-signature look the same
    assert_eq!(body["error"], "invalid or expired token");
    assert!(
        !dir.path().join("out.geojson").exists(),
        "run must not start"
    );
}

#[tokio::test]
async fn token_signed_with_another_secret_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());
    let foreign = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &Claims {
            sub: "user-42".into(),
            exp: usize::MAX,
            role: Some("admin".into()),
            token_use: None,
            scope: None,
        },
        &jsonwebtoken::EncodingKey::from_secret(b"a-completely-different-secret-val"),
    )
    .unwrap();

    let (status, _) = send(gated(), run_request(&manifest, Some(&foreign))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn viewer_role_is_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, body) = send(gated(), run_request(&manifest, Some(&token("viewer", 60)))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "editor or admin role required");
    assert!(
        !dir.path().join("out.geojson").exists(),
        "run must not start"
    );
}

#[tokio::test]
async fn unknown_role_is_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, _) = send(gated(), run_request(&manifest, Some(&token("Editor", 60)))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// No secret configured: /run stays open and the record carries no subject.
#[tokio::test]
async fn dev_mode_runs_without_a_token() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());
    let app = create_router_with_auth(AuthConfig::new(None));

    let (status, record) = send(app, run_request(&manifest, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(record.get("sub").is_none(), "{record}");
}

/// The side-effect-free endpoints headless planning and the eval harness use
/// must not need a token even when the gate is on.
#[tokio::test]
async fn read_only_endpoints_stay_open_when_gated() {
    for uri in ["/health", "/operations"] {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let (status, _) = send(gated(), req).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }

    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());
    let req = Request::builder()
        .method("POST")
        .uri("/validate")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"manifest": manifest}).to_string(),
        ))
        .unwrap();
    let (status, _) = send(gated(), req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn gp_without_a_token_is_rejected() {
    let (status, _) = send(gated(), gp_buffer_request(None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = send(gated(), get_request("/gp/catalog", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "missing bearer token");
}

#[tokio::test]
async fn viewer_cannot_call_gp() {
    let bearer = token("viewer", 60);
    let (status, body) = send(gated(), gp_buffer_request(Some(&bearer))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "editor or admin role required");

    let (status, body) = send(gated(), get_request("/gp/catalog", Some(&bearer))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "editor or admin role required");
}

#[tokio::test]
async fn editor_can_call_gp() {
    let bearer = token("editor", 60);
    let (status, body) = send(gated(), gp_buffer_request(Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tool"], "buffer");

    let (status, _) = send(gated(), get_request("/gp/catalog", Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn scoped_tool_token_can_call_gp() {
    let bearer = tool_token(&["geodukt:run"], 60);
    let (status, _) = send(gated(), gp_buffer_request(Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(gated(), get_request("/gp/catalog", Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn dev_mode_gp_runs_without_a_token() {
    let app = create_router_with_auth(AuthConfig::new(None));

    let (status, _) = send(app.clone(), gp_buffer_request(None)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(app, get_request("/gp/catalog", None)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn run_history_without_a_token_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let app = history_of_two_users(dir.path()).await;

    for uri in ["/runs", "/runs/1"] {
        let (status, body) = send(app.clone(), get_request(uri, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(body["error"], "missing bearer token");
    }
}

#[tokio::test]
async fn expired_token_cannot_read_the_run_history() {
    let dir = tempfile::tempdir().unwrap();
    let app = history_of_two_users(dir.path()).await;

    let (status, _) = send(
        app,
        get_request("/runs", Some(&token_for("user-a", "editor", -3600))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tool_token_cannot_read_the_run_history() {
    let dir = tempfile::tempdir().unwrap();
    let app = history_of_two_users(dir.path()).await;
    let bearer = tool_token(&["geodukt:run"], 60);

    for uri in ["/runs", "/runs/1"] {
        let (status, body) = send(app.clone(), get_request(uri, Some(&bearer))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(body["error"], "platform user token required");
    }
}

#[tokio::test]
async fn a_caller_sees_only_its_own_runs() {
    let dir = tempfile::tempdir().unwrap();
    let app = history_of_two_users(dir.path()).await;
    let bearer = token_for("user-a", "editor", 60);

    let (status, runs) = send(app.clone(), get_request("/runs", Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(subjects(&runs), ["user-a"]);
    assert_eq!(runs[0]["id"], 1);

    let (status, _) = send(app, get_request("/runs/2", Some(&bearer))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_sees_all_callers_runs() {
    let dir = tempfile::tempdir().unwrap();
    let app = history_of_two_users(dir.path()).await;
    let bearer = token_for("the-admin", "admin", 60);

    let (status, runs) = send(app.clone(), get_request("/runs", Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(subjects(&runs), ["user-a", "user-b"]);

    let (status, run) = send(app, get_request("/runs/2", Some(&bearer))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["sub"], "user-b");
}

/// A role the gate does not know reads its own runs, never everyone's.
#[tokio::test]
async fn unknown_role_sees_its_own_runs_only() {
    let dir = tempfile::tempdir().unwrap();
    let app = history_of_two_users(dir.path()).await;

    for role in ["viewer", "auditor", "Admin", ""] {
        let bearer = token_for("user-a", role, 60);
        let (status, runs) = send(app.clone(), get_request("/runs", Some(&bearer))).await;
        assert_eq!(status, StatusCode::OK, "{role}");
        assert_eq!(subjects(&runs), ["user-a"], "{role}");

        let (status, _) = send(app.clone(), get_request("/runs/2", Some(&bearer))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{role}");
    }
}

/// No secret configured: the history stays open, like /run.
#[tokio::test]
async fn dev_mode_reads_the_history_without_a_token() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());
    let app = create_router_with_auth(AuthConfig::new(None));

    let (status, _) = send(app.clone(), run_request(&manifest, None)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, runs) = send(app.clone(), get_request("/runs", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs.as_array().unwrap().len(), 1);

    let (status, _) = send(app, get_request("/runs/1", None)).await;
    assert_eq!(status, StatusCode::OK);
}

/// Without the env var set, the default router is the open dev-mode one.
#[tokio::test]
async fn create_router_without_the_secret_is_open() {
    assert!(
        std::env::var(geodukt_server::auth::SECRET_ENV).is_err(),
        "test env must not set the platform secret"
    );
    let dir = tempfile::tempdir().unwrap();
    let manifest = working_manifest(dir.path());

    let (status, _) = send(create_router(), run_request(&manifest, None)).await;
    assert_eq!(status, StatusCode::OK);
}
