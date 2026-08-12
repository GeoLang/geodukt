//! # geodukt-server
//!
//! REST API for triggering and monitoring geodukt pipelines.

pub mod auth;
pub mod gp_tools;
pub mod runs;
pub mod validate;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use auth::{AuthConfig, Caller};
use runs::now_rfc3339;
pub use runs::{RunRecord, RunStatus, RunStore, StepRecord, StepStatus};

use geodukt_core::manifest::Manifest;
use geodukt_core::pipeline::{Pipeline, StepResult};
use geodukt_io::formats::{FormatSpec, MultiFormatReader, MultiFormatWriter, formats};
use geodukt_transforms::registry::{OperationSpec, default_registry, operations};

/// Shared server state.
#[derive(Clone)]
struct AppState {
    runs: RunStore,
    auth: AuthConfig,
}

/// Request to trigger a pipeline run.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub manifest: String,
}

/// Create the server router, reading the platform secret from the environment.
pub fn create_router() -> Router {
    create_router_with_auth(AuthConfig::from_env())
}

/// Create the server router with an explicit auth config, over the run history
/// named by [`runs::RUNS_DB_ENV`].
pub fn create_router_with_auth(auth: AuthConfig) -> Router {
    let runs = RunStore::from_env().expect("could not open the run history database");
    create_router_with_store(auth, runs)
}

/// Create the server router over an already opened run history.
pub fn create_router_with_store(auth: AuthConfig, runs: RunStore) -> Router {
    let state = AppState {
        runs,
        auth: auth.clone(),
    };

    Router::new()
        .route("/health", get(health))
        .route("/operations", get(list_operations))
        .route("/validate", post(validate_manifest))
        .route(
            "/run",
            post(trigger_run).layer(from_fn_with_state(auth.clone(), auth::require_run_access)),
        )
        .route(
            "/runs",
            get(list_runs).layer(from_fn_with_state(
                auth.clone(),
                auth::require_history_access,
            )),
        )
        .route(
            "/runs/{id}",
            get(get_run).layer(from_fn_with_state(auth, auth::require_history_access)),
        )
        .nest("/gp", gp_tools::gp_routes().with_state(()))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

/// What a manifest may name: every transform operation and every source or sink
/// format. Both lists come from the tables the engine dispatches on.
#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub operations: Vec<OperationSpec>,
    pub formats: &'static [FormatSpec],
}

async fn list_operations() -> Json<Catalog> {
    Json(Catalog {
        operations: operations(),
        formats: formats(),
    })
}

async fn validate_manifest(
    Json(req): Json<RunRequest>,
) -> Result<Json<validate::Plan>, (StatusCode, Json<validate::Problem>)> {
    validate::validate_manifest(&req.manifest)
        .map(Json)
        .map_err(|problem| (problem.status(), Json(problem)))
}

/// Why a run request did not produce a completed run.
enum RunError {
    /// The body is not a manifest that can be turned into a pipeline, so there
    /// is nothing to record.
    BadRequest(String),
    /// The manifest describes work the engine cannot carry out, caught before
    /// anything ran, so there is no attempt to record. Same body as `/validate`.
    Rejected(validate::Problem),
    /// The pipeline ran and failed. The attempt is recorded, and the record
    /// comes back so the caller has the id and the reason.
    Failed(Box<RunRecord>),
    /// The pipeline ran and the attempt could not be stored, so there is no
    /// record to hand back and nothing `/runs` will show.
    NotRecorded,
}

impl IntoResponse for RunError {
    fn into_response(self) -> Response {
        match self {
            RunError::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            RunError::Rejected(problem) => (problem.status(), Json(problem)).into_response(),
            // the manifest was well formed and the work it described could not be
            // carried out, which is the request's content rather than a server
            // fault, so 422 rather than 500. a 500 would tell a client to retry
            // something that cannot succeed.
            RunError::Failed(record) => {
                (StatusCode::UNPROCESSABLE_ENTITY, Json(record)).into_response()
            }
            RunError::NotRecorded => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not store the run record",
            )
                .into_response(),
        }
    }
}

async fn trigger_run(
    State(state): State<AppState>,
    caller: Caller,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunRecord>, RunError> {
    let manifest = Manifest::from_toml(&req.manifest)
        .map_err(|e| RunError::BadRequest(format!("Invalid manifest: {e}")))?;

    let pipeline = Pipeline::new(manifest.clone())
        .map_err(|e| RunError::BadRequest(format!("Pipeline error: {e}")))?;

    validate::check_missing_parameters(&manifest).map_err(RunError::Rejected)?;

    let name = manifest.project.name;
    let started_at = now_rfc3339();

    // a run reads whole files and computes inline, so it would hold an async
    // worker for as long as it takes
    let outcome = tokio::task::spawn_blocking(move || {
        pipeline.execute(&MultiFormatReader, &default_registry(), &MultiFormatWriter)
    })
    .await
    .expect("pipeline run panicked");

    match outcome {
        Ok(report) => {
            let steps = report.steps.iter().map(completed_step).collect();
            let record = state
                .runs
                .record(
                    RunStatus::Completed,
                    name,
                    req.manifest,
                    steps,
                    caller.sub(),
                    started_at,
                )
                .map_err(|_| RunError::NotRecorded)?;
            Ok(Json(record))
        }
        Err(failure) => {
            let mut steps: Vec<StepRecord> = failure.completed.iter().map(completed_step).collect();
            if let Some(failed) = &failure.failed {
                steps.push(StepRecord {
                    name: failed.name.clone(),
                    feature_count: 0,
                    status: StepStatus::Failed(failed.message.clone()),
                });
            }
            steps.extend(failure.not_run.iter().map(|name| StepRecord {
                name: name.clone(),
                feature_count: 0,
                status: StepStatus::NotRun,
            }));

            let record = state
                .runs
                .record(
                    RunStatus::Failed(format!("Execution error: {}", failure.error())),
                    name,
                    req.manifest,
                    steps,
                    caller.sub(),
                    started_at,
                )
                .map_err(|_| RunError::NotRecorded)?;
            Err(RunError::Failed(Box::new(record)))
        }
    }
}

fn completed_step(step: &StepResult) -> StepRecord {
    StepRecord {
        name: step.name.clone(),
        feature_count: step.feature_count,
        status: StepStatus::Completed,
    }
}

async fn list_runs(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<Vec<RunRecord>>, StatusCode> {
    let visible = state
        .auth
        .run_visibility(&caller)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    state
        .runs
        .list(visible.required_subject())
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_run(
    State(state): State<AppState>,
    caller: Caller,
    axum::extract::Path(id): axum::extract::Path<usize>,
) -> Result<Json<RunRecord>, StatusCode> {
    let visible = state
        .auth
        .run_visibility(&caller)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // another caller's run answers 404 like a missing one, so ids cannot be probed
    state
        .runs
        .get(id, visible.required_subject())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// Start the server on the given address.
pub async fn serve(bind: &str) -> std::io::Result<()> {
    let router = create_router();
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router.into_make_service())
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
}
