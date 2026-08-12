//! Tests for the REST API server.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use geodukt_server::auth::AuthConfig;
use geodukt_server::{
    RunRecord, RunStatus, RunStore, StepStatus, create_router, create_router_with_store,
};
use tower::ServiceExt;

#[tokio::test]
async fn test_health_endpoint() {
    let app = create_router();
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn test_list_runs_empty() {
    let app = create_router();
    let req = Request::builder().uri("/runs").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json, serde_json::json!([]));
}

#[tokio::test]
async fn test_get_run_not_found() {
    let app = create_router();
    for id in ["999", "18446744073709551615"] {
        let req = Request::builder()
            .uri(format!("/runs/{id}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{id}");
    }
}

#[tokio::test]
async fn test_run_invalid_manifest() {
    let app = create_router();
    let req = Request::builder()
        .method("POST")
        .uri("/run")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"manifest": "not valid toml [["}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Post a JSON body and return the status plus the decoded response.
async fn post(uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    request(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn get(uri: &str) -> (StatusCode, serde_json::Value) {
    request(Request::builder().uri(uri).body(Body::empty()).unwrap()).await
}

async fn request(req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = create_router().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

const PLAN: &str = r#"
[project]
name = "city"
version = "1.2.0"

[[source]]
name = "parcels"
format = "gpkg"
path = "data/city.gpkg"
layer = "parcels"

[[transform]]
name = "centers"
input = "parcels"
operation = "centroid"

[[sink]]
name = "out"
input = "centers"
format = "csv"
path = "out/centers.csv"
"#;

#[tokio::test]
async fn test_validate_returns_the_step_order_and_details() {
    let (status, plan) = post("/validate", serde_json::json!({"manifest": PLAN})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["project"], "city");
    assert_eq!(plan["version"], "1.2.0");

    let steps = plan["steps"].as_array().unwrap();
    let names: Vec<&str> = steps.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["parcels", "centers", "out"]);

    assert_eq!(steps[0]["kind"], "source");
    assert_eq!(steps[0]["format"], "gpkg");
    assert_eq!(steps[0]["path"], "data/city.gpkg");
    assert_eq!(steps[0]["layer"], "parcels");
    // fields that do not apply to a source are absent, not null
    assert!(steps[0].get("operation").is_none());

    assert_eq!(steps[1]["kind"], "transform");
    assert_eq!(steps[1]["operation"], "centroid");
    assert_eq!(steps[1]["input"], "parcels");

    assert_eq!(steps[2]["kind"], "sink");
    assert_eq!(steps[2]["input"], "centers");
    assert_eq!(steps[2]["format"], "csv");
}

#[tokio::test]
async fn test_validate_does_not_execute() {
    // the manifest names paths that do not exist, so a run would fail
    let (status, _) = post("/validate", serde_json::json!({"manifest": PLAN})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!std::path::Path::new("out/centers.csv").exists());

    let (runs_status, runs) = get("/runs").await;
    assert_eq!(runs_status, StatusCode::OK);
    assert_eq!(runs, serde_json::json!([]), "validating must record no run");
}

#[tokio::test]
async fn test_validate_toml_error_is_400_with_kind_toml() {
    let (status, problem) = post(
        "/validate",
        serde_json::json!({"manifest": "[project\nbroken"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["kind"], "toml");
    assert!(problem["message"].as_str().unwrap().len() > 5);
}

#[tokio::test]
async fn test_validate_graph_error_is_422_with_kind_graph() {
    let manifest = r#"
[project]
name = "broken"

[[transform]]
name = "orphan"
input = "missing"
operation = "centroid"
"#;
    let (status, problem) = post("/validate", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["kind"], "graph");
    assert!(
        problem["message"].as_str().unwrap().contains("missing"),
        "{problem}"
    );
}

#[tokio::test]
async fn test_validate_unknown_operation_is_422_with_kind_operation() {
    let manifest = r#"
[project]
name = "broken"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "oops"
input = "src"
operation = "buffer_it_up"
"#;
    let (status, problem) = post("/validate", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["kind"], "operation");
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("buffer_it_up"),
        "{problem}"
    );
}

#[tokio::test]
async fn test_validate_unknown_format_is_422_with_kind_format() {
    let manifest = r#"
[project]
name = "broken"

[[source]]
name = "raster"
format = "geotiff"
path = "a.tif"
"#;
    let (status, problem) = post("/validate", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem["kind"], "format");
}

#[tokio::test]
async fn test_operations_catalog_matches_the_registry_table() {
    let (status, catalog) = get("/operations").await;
    assert_eq!(status, StatusCode::OK);

    let listed: Vec<&str> = catalog["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["name"].as_str().unwrap())
        .collect();
    let expected: Vec<&str> = geodukt_transforms::registry::operations()
        .iter()
        .map(|op| op.name)
        .collect();
    assert_eq!(listed, expected);
    assert!(listed.len() > 5, "more than the 5 gp tools: {listed:?}");
    assert!(listed.contains(&"schema_map"));
    assert!(listed.contains(&"expression"));
}

#[tokio::test]
async fn test_operations_catalog_carries_parameter_specs() {
    let (_, catalog) = get("/operations").await;
    let ops = catalog["operations"].as_array().unwrap();

    let buffer = ops.iter().find(|op| op["name"] == "buffer").unwrap();
    assert!(!buffer["description"].as_str().unwrap().is_empty());
    let distance = buffer["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "distance")
        .unwrap();
    assert_eq!(distance["param_type"], "float");
    // a buffer with no distance is not a buffer, so there is nothing to default to
    assert_eq!(distance["required"], true);
    assert!(distance.get("default").is_none(), "{distance}");

    // segments is a quality knob, so it keeps its default
    let segments = buffer["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "segments")
        .unwrap();
    assert_eq!(segments["required"], false);
    assert_eq!(segments["default"], "64");

    // the real parameter names, not the ones /gp/catalog used to advertise
    let simplify = ops.iter().find(|op| op["name"] == "simplify").unwrap();
    assert_eq!(simplify["parameters"][0]["name"], "epsilon");
    let dissolve = ops.iter().find(|op| op["name"] == "dissolve").unwrap();
    assert_eq!(dissolve["parameters"][0]["name"], "group_by");
}

/// The catalogue is what the model composing a manifest reads, so a parameter it
/// has to supply says so there.
#[tokio::test]
async fn test_operations_catalog_reports_required_parameters() {
    let (_, catalog) = get("/operations").await;
    let ops = catalog["operations"].as_array().unwrap();

    let required = |op_name: &str, param: &str| -> serde_json::Value {
        let op = ops.iter().find(|op| op["name"] == op_name).unwrap();
        op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == param)
            .unwrap_or_else(|| panic!("{op_name} has no parameter {param}"))
            .clone()
    };

    for (op, param) in [
        ("buffer", "distance"),
        ("simplify", "epsilon"),
        ("filter", "field"),
        ("filter", "equals"),
        ("clip", "min_x"),
        ("clip", "min_y"),
        ("clip", "max_x"),
        ("clip", "max_y"),
        ("expression", "expressions"),
    ] {
        let spec = required(op, param);
        assert_eq!(spec["required"], true, "{op}.{param}: {spec}");
        // a required parameter cannot also have a value that stands in for it
        assert!(spec.get("default").is_none(), "{op}.{param}: {spec}");
    }

    // optional by decision: autodetecting the source CRS and dissolving
    // everything into one shape are both real requests
    assert_eq!(required("reproject", "from_crs")["required"], false);
    assert_eq!(required("dissolve", "group_by")["required"], false);

    // schema_map needs one of three rather than any particular one
    let schema_map = ops.iter().find(|op| op["name"] == "schema_map").unwrap();
    for param in ["rename", "drop", "add"] {
        assert_eq!(required("schema_map", param)["required"], false);
    }
    assert_eq!(
        schema_map["requires_any"]["parameters"],
        serde_json::json!(["rename", "drop", "add"])
    );
    assert!(
        schema_map["requires_any"]["purpose"].is_string(),
        "{schema_map}"
    );
}

#[tokio::test]
async fn test_operations_catalog_requires_the_target_crs() {
    let (_, catalog) = get("/operations").await;
    let ops = catalog["operations"].as_array().unwrap();
    let reproject = ops.iter().find(|op| op["name"] == "reproject").unwrap();
    let to_crs = reproject["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "to_crs")
        .unwrap();
    assert_eq!(to_crs["required"], true);
    assert!(to_crs.get("default").is_none(), "{to_crs}");
}

#[tokio::test]
async fn test_operations_catalog_flags_what_cannot_run() {
    let (_, catalog) = get("/operations").await;
    let ops = catalog["operations"].as_array().unwrap();

    let join = ops.iter().find(|op| op["name"] == "spatial_join").unwrap();
    assert!(join["unavailable"].is_string(), "{join}");

    let centroid = ops.iter().find(|op| op["name"] == "centroid").unwrap();
    assert!(centroid.get("unavailable").is_none(), "{centroid}");
}

#[tokio::test]
async fn test_operations_catalog_lists_formats_and_their_fields() {
    let (_, catalog) = get("/operations").await;
    let formats = catalog["formats"].as_array().unwrap();

    let names: Vec<&str> = formats
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["csv", "geojson", "geopackage", "shapefile"]);

    let gpkg = formats.iter().find(|f| f["name"] == "geopackage").unwrap();
    assert_eq!(gpkg["aliases"], serde_json::json!(["gpkg"]));
    assert_eq!(gpkg["reads"], true);
    assert_eq!(gpkg["writes"], true);
    assert_eq!(gpkg["fields"], serde_json::json!(["path", "layer"]));

    let geojson = formats.iter().find(|f| f["name"] == "geojson").unwrap();
    assert_eq!(geojson["fields"], serde_json::json!(["path"]));
}

#[tokio::test]
async fn test_run_record_carries_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.geojson");
    std::fs::write(
        &input,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"a"},
             "geometry":{"type":"Point","coordinates":[1.0,2.0]}}]}"#,
    )
    .unwrap();

    let manifest = format!(
        r#"
[project]
name = "reproducible"

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
        output = dir.path().join("out.geojson").display()
    );

    // one router so the run and the lookup share state
    let app = create_router();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/run")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"manifest": manifest}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["manifest_name"], "reproducible");
    assert_eq!(record["manifest"], manifest);

    // and the stored record still has it, so a past run can be repeated
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/runs/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stored["id"], 1);
    assert_eq!(stored["manifest"], manifest);

    // the stored text is a manifest the validator accepts, so it is replayable
    let (status, _) = post("/validate", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::OK);
}

/// A buffer with no distance used to run with a 1 metre default, so the caller
/// got a buffer nothing asked for. Both entry points reject it now, and with the
/// same message, since whatever composed the manifest reads it and retries.
#[tokio::test]
async fn test_run_rejects_a_transform_missing_a_required_parameter() {
    let manifest = r#"
[project]
name = "silent"

[[source]]
name = "pts"
format = "geojson"
path = "pts.geojson"

[[transform]]
name = "wide"
input = "pts"
operation = "buffer"

[[sink]]
name = "out"
input = "wide"
format = "geojson"
path = "out.geojson"
"#;

    let app = create_router();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/run")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"manifest": manifest}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let problem: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem["kind"], "operation");
    assert!(
        problem["message"].as_str().unwrap().contains("distance"),
        "{problem}"
    );

    let (_, from_validate) = post("/validate", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(from_validate["message"], problem["message"]);

    // nothing ran, so there is no attempt to record
    let resp = app
        .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"[]");
}

/// A manifest that parses and builds a valid DAG but cannot run: the CSV sink
/// only carries point geometry, and the source hands it a polygon.
fn failing_manifest(dir: &std::path::Path) -> String {
    let input = dir.join("polys.geojson");
    std::fs::write(
        &input,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"id":1},
             "geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}]}"#,
    )
    .unwrap();

    format!(
        r#"
[project]
name = "doomed"

[[source]]
name = "polys"
format = "geojson"
path = "{input}"

[[sink]]
name = "out"
input = "polys"
format = "csv"
path = "{output}"
"#,
        input = input.display(),
        output = dir.join("out.csv").display()
    )
}

#[tokio::test]
async fn test_failed_run_returns_422_with_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = failing_manifest(dir.path());

    let (status, record) = post("/run", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // the failure body is a run record, so a caller parses one shape either way
    assert_eq!(record["id"], 1);
    assert_eq!(record["manifest_name"], "doomed");

    // the run got as far as reading the source, and died on the sink
    let steps = record["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["name"], "polys");
    assert_eq!(steps[0]["status"], "Completed");
    assert_eq!(steps[0]["feature_count"], 1);
    assert_eq!(steps[1]["name"], "out");
    let step_error = steps[1]["status"]["Failed"].as_str().unwrap();
    assert!(
        step_error.contains("cannot write a Polygon"),
        "{step_error}"
    );

    let reason = record["status"]["Failed"].as_str().unwrap();
    assert!(reason.contains("cannot write a Polygon"), "{reason}");
    // the message names the step that failed
    assert!(reason.contains("out"), "{reason}");
}

#[tokio::test]
async fn test_failure_mid_pipeline_marks_later_steps_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("pts.geojson");
    std::fs::write(
        &input,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"id":1},
             "geometry":{"type":"Point","coordinates":[0,0]}}]}"#,
    )
    .unwrap();

    // spatial_join is registered but has no join dataset, so it always fails
    let manifest = format!(
        r#"
[project]
name = "midway"

[[source]]
name = "pts"
format = "geojson"
path = "{input}"

[[transform]]
name = "joined"
input = "pts"
operation = "spatial_join"

[[sink]]
name = "out"
input = "joined"
format = "geojson"
path = "{output}"
"#,
        input = input.display(),
        output = dir.path().join("out.geojson").display()
    );

    let (status, record) = post("/run", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let steps = record["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["name"], "pts");
    assert_eq!(steps[0]["status"], "Completed");
    assert_eq!(steps[1]["name"], "joined");
    assert!(steps[1]["status"]["Failed"].is_string(), "{record}");
    assert_eq!(steps[2]["name"], "out");
    assert_eq!(steps[2]["status"], "NotRun");
}

/// Records written before steps carried a status only ever came from runs that
/// completed, so they read back as completed steps.
#[test]
fn test_old_record_without_step_status_deserializes() {
    let old = r#"{
        "id": 3,
        "status": "Completed",
        "manifest_name": "legacy",
        "manifest": "[project]\nname = \"legacy\"\n",
        "steps": [{"name": "src", "feature_count": 7}],
        "started_at": "2026-08-12T09:00:00.000Z",
        "finished_at": "2026-08-12T09:00:01.000Z"
    }"#;

    let record: RunRecord = serde_json::from_str(old).unwrap();
    assert_eq!(record.id, 3);
    assert_eq!(record.sub, None);
    assert_eq!(record.steps[0].feature_count, 7);
    assert_eq!(record.steps[0].status, StepStatus::Completed);
}

#[tokio::test]
async fn test_failed_run_is_listed_and_replayable() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = failing_manifest(dir.path());
    let app = create_router();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/run")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"manifest": manifest}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // GET /runs/{id} shows the failed run
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/runs/1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(stored["status"]["Failed"].is_string(), "{stored}");
    assert_eq!(stored["manifest"], manifest);

    // and GET /runs lists it
    let resp = app
        .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let runs: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["id"], 1);

    // the stored manifest still parses, so the failure can be reproduced
    let (status, _) = post("/validate", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_completed_status_shape_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.geojson");
    std::fs::write(
        &input,
        r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{},
             "geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
    )
    .unwrap();

    let manifest = format!(
        r#"
[project]
name = "fine"

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
        output = dir.path().join("out.geojson").display()
    );

    let (status, record) = post("/run", serde_json::json!({"manifest": manifest})).await;
    assert_eq!(status, StatusCode::OK);
    // a completed run still serializes status as the bare string "Completed"
    assert_eq!(record["status"], "Completed");
    assert_eq!(record["steps"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_run_ids_keep_counting_across_failures() {
    let dir = tempfile::tempdir().unwrap();
    let failing = failing_manifest(dir.path());
    let app = create_router();

    for expected_id in 1..3 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/run")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"manifest": failing}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record["id"], expected_id);
    }
}

/// Runs a point through to a geojson sink, so the run completes.
fn completing_manifest(dir: &std::path::Path) -> String {
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
name = "kept"

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

/// A restart is a fresh router over the same database, and the run is still
/// there with the times it was stamped with.
#[tokio::test]
async fn test_runs_outlive_the_router_that_recorded_them() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("runs.sqlite");
    let database = database.to_str().unwrap().to_string();
    let manifest = completing_manifest(dir.path());

    let router = || {
        create_router_with_store(
            AuthConfig::new(None),
            RunStore::open(Some(&database)).unwrap(),
        )
    };

    let resp = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/run")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"manifest": manifest}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let recorded: RunRecord = serde_json::from_slice(&bytes).unwrap();

    let resp = router()
        .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let runs: Vec<RunRecord> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, 1);
    assert_eq!(runs[0].manifest_name, "kept");
    assert_eq!(runs[0].status, RunStatus::Completed);
    assert_eq!(runs[0].manifest, manifest);
    assert_eq!(runs[0].started_at, recorded.started_at);
    assert_eq!(runs[0].finished_at, recorded.finished_at);

    // the next run carries on from the stored ids
    let resp = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/run")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"manifest": manifest}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let second: RunRecord = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(second.id, 2);
}

#[tokio::test]
async fn test_unparseable_manifest_records_no_run() {
    let app = create_router();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/run")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"manifest": "[project\nbroken"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // nothing was attempted, so there is nothing to record
    let resp = app
        .oneshot(Request::builder().uri("/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"[]");
}
