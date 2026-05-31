//! # geodukt-server
//!
//! REST API for triggering and monitoring geodukt pipelines.

pub mod gp_tools;

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use geodukt_core::manifest::Manifest;
use geodukt_core::pipeline::Pipeline;
use geodukt_io::geojson_io::{MultiFormatReader, MultiFormatWriter};
use geodukt_transforms::registry::default_registry;

/// Shared server state.
#[derive(Clone)]
struct AppState {
    runs: Arc<Mutex<Vec<RunRecord>>>,
}

/// Record of a pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: usize,
    pub status: RunStatus,
    pub manifest_name: String,
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

async fn trigger_run(
    State(state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunRecord>, (StatusCode, String)> {
    let manifest = Manifest::from_toml(&req.manifest)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid manifest: {e}")))?;

    let pipeline = Pipeline::new(manifest.clone())
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Pipeline error: {e}")))?;

    let transforms = default_registry();
    let reader = MultiFormatReader;
    let writer = MultiFormatWriter;

    let report = pipeline
        .execute(&reader, &transforms, &writer)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Execution error: {e}"),
            )
        })?;

    let steps: Vec<StepRecord> = report
        .steps
        .iter()
        .map(|s| StepRecord {
            name: s.name.clone(),
            feature_count: s.feature_count,
        })
        .collect();

    let mut runs = state.runs.lock().unwrap();
    let id = runs.len();
    let record = RunRecord {
        id,
        status: RunStatus::Completed,
        manifest_name: manifest.project.name,
        steps,
    };
    runs.push(record.clone());

    Ok(Json(record))
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
