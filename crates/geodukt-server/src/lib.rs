//! # geodukt-server
//!
//! REST API for triggering and monitoring geodukt pipelines.

pub mod gp_tools;
pub mod validate;

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use geodukt_core::manifest::Manifest;
use geodukt_core::pipeline::Pipeline;
use geodukt_io::formats::{FormatSpec, MultiFormatReader, MultiFormatWriter, formats};
use geodukt_transforms::registry::{OperationSpec, default_registry, operations};

/// Shared server state.
#[derive(Clone)]
struct AppState {
    runs: Arc<Mutex<Vec<RunRecord>>>,
}

impl AppState {
    /// Append a run attempt, completed or failed, and hand back the stored record.
    fn record(
        &self,
        status: RunStatus,
        manifest_name: String,
        manifest: String,
        steps: Vec<StepRecord>,
    ) -> RunRecord {
        let mut runs = self.runs.lock().unwrap();
        let record = RunRecord {
            id: runs.len(),
            status,
            manifest_name,
            manifest,
            steps,
        };
        runs.push(record.clone());
        record
    }
}

/// Record of a pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: usize,
    pub status: RunStatus,
    pub manifest_name: String,
    /// The manifest TOML exactly as submitted, so the run can be repeated.
    pub manifest: String,
    pub steps: Vec<StepRecord>,
}

/// Step record for API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub name: String,
    pub feature_count: usize,
}

/// Pipeline run status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed(String),
}

/// Request to trigger a pipeline run.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub manifest: String,
}

/// Create the server router.
pub fn create_router() -> Router {
    let state = AppState {
        runs: Arc::new(Mutex::new(Vec::new())),
    };

    Router::new()
        .route("/health", get(health))
        .route("/operations", get(list_operations))
        .route("/validate", post(validate_manifest))
        .route("/run", post(trigger_run))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
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
    /// The pipeline ran and failed. The attempt is recorded, and the record
    /// comes back so the caller has the id and the reason.
    Failed(Box<RunRecord>),
}

impl IntoResponse for RunError {
    fn into_response(self) -> Response {
        match self {
            RunError::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            // the manifest was well formed and the work it described could not be
            // carried out, which is the request's content rather than a server
            // fault, so 422 rather than 500. a 500 would tell a client to retry
            // something that cannot succeed.
            RunError::Failed(record) => {
                (StatusCode::UNPROCESSABLE_ENTITY, Json(record)).into_response()
            }
        }
    }
}

async fn trigger_run(
    State(state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunRecord>, RunError> {
    let manifest = Manifest::from_toml(&req.manifest)
        .map_err(|e| RunError::BadRequest(format!("Invalid manifest: {e}")))?;

    let pipeline = Pipeline::new(manifest.clone())
        .map_err(|e| RunError::BadRequest(format!("Pipeline error: {e}")))?;

    let transforms = default_registry();
    let reader = MultiFormatReader;
    let writer = MultiFormatWriter;
    let name = manifest.project.name;

    match pipeline.execute(&reader, &transforms, &writer) {
        Ok(report) => {
            let steps = report
                .steps
                .iter()
                .map(|s| StepRecord {
                    name: s.name.clone(),
                    feature_count: s.feature_count,
                })
                .collect();
            Ok(Json(state.record(
                RunStatus::Completed,
                name,
                req.manifest,
                steps,
            )))
        }
        // execute drops its progress on error, so a failed run records no steps.
        // the message names the step that failed.
        Err(e) => Err(RunError::Failed(Box::new(state.record(
            RunStatus::Failed(format!("Execution error: {e}")),
            name,
            req.manifest,
            Vec::new(),
        )))),
    }
}

async fn list_runs(State(state): State<AppState>) -> Json<Vec<RunRecord>> {
    let runs = state.runs.lock().unwrap();
    Json(runs.clone())
}

async fn get_run(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<usize>,
) -> Result<Json<RunRecord>, StatusCode> {
    let runs = state.runs.lock().unwrap();
    runs.get(id).cloned().map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// Start the server on the given address.
pub async fn serve(bind: &str) -> std::io::Result<()> {
    let router = create_router();
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router.into_make_service())
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
}
