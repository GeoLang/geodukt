//! Manifest validation — check a plan without running it.
//!
//! A caller composing manifests needs to know which part is wrong, so every
//! failure carries a [`ProblemKind`] alongside the message. The checks run in
//! the order a run would hit them: parse the TOML, build the graph, then
//! confirm every operation and format the manifest names actually exists.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use geodukt_core::dag::Node;
use geodukt_core::manifest::Manifest;
use geodukt_core::pipeline::Pipeline;
use geodukt_io::formats::format;
use geodukt_transforms::registry::{check_parameters, operation};

/// Which part of a manifest a caller has to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// The body is not valid TOML, or does not match the manifest schema.
    Toml,
    /// The TOML parsed but the graph does not hold together: an unknown input,
    /// a duplicate node name, or a cycle.
    Graph,
    /// A transform names an operation the engine does not have, or cannot run.
    Operation,
    /// A source or sink names a format that is not wired up.
    Format,
}

/// A single reason a manifest was rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    pub kind: ProblemKind,
    pub message: String,
}

impl Problem {
    fn new(kind: ProblemKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// TOML problems are a malformed request body, everything else is a
    /// well-formed body describing a plan that cannot run.
    pub fn status(&self) -> StatusCode {
        match self.kind {
            ProblemKind::Toml => StatusCode::BAD_REQUEST,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

/// A validated plan: the steps in the order the executor would run them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub project: String,
    pub version: String,
    pub steps: Vec<PlanStep>,
}

/// What a node in the graph is and does. Fields that do not apply to the kind
/// are left out rather than sent as null.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub name: String,
    pub kind: StepKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// Operation parameters as given in the manifest, for transform steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Source,
    Transform,
    Sink,
}

/// Parse and check a manifest without executing it, returning the plan the
/// executor would run.
pub fn validate_manifest(toml: &str) -> Result<Plan, Problem> {
    let manifest =
        Manifest::from_toml(toml).map_err(|e| Problem::new(ProblemKind::Toml, e.to_string()))?;

    // Pipeline::new builds the DAG, which is where cycles and unknown or
    // duplicated node names surface.
    let pipeline = Pipeline::new(manifest.clone())
        .map_err(|e| Problem::new(ProblemKind::Graph, e.to_string()))?;

    check_operations(&manifest)?;
    check_formats(&manifest)?;

    let steps = pipeline
        .plan()
        .map_err(|e| Problem::new(ProblemKind::Graph, e.to_string()))?
        .into_iter()
        .map(plan_step)
        .collect();

    Ok(Plan {
        project: manifest.project.name,
        version: manifest.project.version,
        steps,
    })
}

fn check_operations(manifest: &Manifest) -> Result<(), Problem> {
    for transform in &manifest.transform {
        let Some(spec) = operation(&transform.operation) else {
            return Err(Problem::new(
                ProblemKind::Operation,
                format!(
                    "transform '{}' names unknown operation '{}', see GET /operations",
                    transform.name, transform.operation
                ),
            ));
        };
        if let Some(reason) = spec.unavailable {
            return Err(Problem::new(
                ProblemKind::Operation,
                format!(
                    "transform '{}' uses operation '{}' which cannot run: {reason}",
                    transform.name, transform.operation
                ),
            ));
        }
    }
    check_missing_parameters(manifest)
}

/// Reject a transform that leaves out a parameter its operation cannot run
/// without. Public because `/run` has to reject before it executes anything,
/// with the message `/validate` gives.
pub fn check_missing_parameters(manifest: &Manifest) -> Result<(), Problem> {
    check_parameters(manifest).map_err(|message| Problem::new(ProblemKind::Operation, message))
}

fn check_formats(manifest: &Manifest) -> Result<(), Problem> {
    for source in &manifest.source {
        match format(&source.format) {
            Some(spec) if spec.reads => {}
            Some(spec) => {
                return Err(Problem::new(
                    ProblemKind::Format,
                    format!(
                        "source '{}' uses format '{}', which cannot be read",
                        source.name, spec.name
                    ),
                ));
            }
            None => {
                return Err(Problem::new(
                    ProblemKind::Format,
                    format!(
                        "source '{}' names unknown format '{}', see GET /operations",
                        source.name, source.format
                    ),
                ));
            }
        }
    }

    for sink in &manifest.sink {
        match format(&sink.format) {
            Some(spec) if spec.writes => {}
            Some(spec) => {
                return Err(Problem::new(
                    ProblemKind::Format,
                    format!(
                        "sink '{}' uses format '{}', which cannot be written",
                        sink.name, spec.name
                    ),
                ));
            }
            None => {
                return Err(Problem::new(
                    ProblemKind::Format,
                    format!(
                        "sink '{}' names unknown format '{}', see GET /operations",
                        sink.name, sink.format
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn plan_step(node: &Node) -> PlanStep {
    match node {
        Node::Source(source) => PlanStep {
            name: source.name.clone(),
            kind: StepKind::Source,
            operation: None,
            input: None,
            format: Some(source.format.clone()),
            path: Some(source.path.clone()),
            layer: source.layer.clone(),
            crs: source.crs.clone(),
            params: None,
        },
        Node::Transform(transform) => PlanStep {
            name: transform.name.clone(),
            kind: StepKind::Transform,
            operation: Some(transform.operation.clone()),
            input: Some(transform.input.clone()),
            format: None,
            path: None,
            layer: None,
            crs: None,
            params: Some(serde_json::to_value(&transform.params).unwrap_or_default()),
        },
        Node::Sink(sink) => PlanStep {
            name: sink.name.clone(),
            kind: StepKind::Sink,
            operation: None,
            input: Some(sink.input.clone()),
            format: Some(sink.format.clone()),
            path: Some(sink.path.clone()),
            layer: sink.layer.clone(),
            crs: None,
            params: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[project]
name = "city"
version = "2.0.0"

[[source]]
name = "parcels"
format = "gpkg"
path = "data/city.gpkg"
layer = "parcels"
crs = "EPSG:4326"

[[transform]]
name = "centers"
input = "parcels"
operation = "centroid"

[[sink]]
name = "out"
input = "centers"
format = "geojson"
path = "out/centers.geojson"
"#;

    #[test]
    fn test_valid_manifest_returns_execution_order() {
        let plan = validate_manifest(GOOD).unwrap();
        assert_eq!(plan.project, "city");
        assert_eq!(plan.version, "2.0.0");

        let names: Vec<&str> = plan.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["parcels", "centers", "out"]);

        let source = &plan.steps[0];
        assert_eq!(source.kind, StepKind::Source);
        assert_eq!(source.format.as_deref(), Some("gpkg"));
        assert_eq!(source.path.as_deref(), Some("data/city.gpkg"));
        assert_eq!(source.layer.as_deref(), Some("parcels"));
        assert_eq!(source.crs.as_deref(), Some("EPSG:4326"));
        assert!(source.operation.is_none());

        let transform = &plan.steps[1];
        assert_eq!(transform.kind, StepKind::Transform);
        assert_eq!(transform.operation.as_deref(), Some("centroid"));
        assert_eq!(transform.input.as_deref(), Some("parcels"));

        let sink = &plan.steps[2];
        assert_eq!(sink.kind, StepKind::Sink);
        assert_eq!(sink.input.as_deref(), Some("centers"));
        assert_eq!(sink.format.as_deref(), Some("geojson"));
    }

    #[test]
    fn test_transform_params_are_reported() {
        let plan = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "wide"
input = "src"
operation = "buffer"
distance = 25.0
"#,
        )
        .unwrap();
        let params = plan.steps[1].params.as_ref().unwrap();
        assert_eq!(params["distance"], serde_json::json!(25.0));
    }

    #[test]
    fn test_toml_syntax_error_is_a_toml_problem() {
        let problem = validate_manifest("[project\nname = broken").unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Toml);
        assert_eq!(problem.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_missing_required_field_is_a_toml_problem() {
        // parses as TOML but does not match the manifest schema
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
path = "a.geojson"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Toml);
        assert!(problem.message.contains("format"), "{}", problem.message);
    }

    #[test]
    fn test_unknown_input_is_a_graph_problem() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[transform]]
name = "orphan"
input = "nothing"
operation = "centroid"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Graph);
        assert_eq!(problem.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(problem.message.contains("nothing"), "{}", problem.message);
    }

    #[test]
    fn test_cycle_is_a_graph_problem() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[transform]]
name = "a"
input = "b"
operation = "centroid"

[[transform]]
name = "b"
input = "a"
operation = "centroid"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Graph);
        assert!(problem.message.contains("cycle"), "{}", problem.message);
    }

    #[test]
    fn test_duplicate_name_is_a_graph_problem() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[source]]
name = "src"
format = "geojson"
path = "b.geojson"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Graph);
        assert!(problem.message.contains("duplicate"), "{}", problem.message);
    }

    #[test]
    fn test_unknown_operation_is_an_operation_problem() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "oops"
input = "src"
operation = "buffr"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Operation);
        assert_eq!(problem.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(problem.message.contains("buffr"), "{}", problem.message);
    }

    #[test]
    fn test_unavailable_operation_is_rejected() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "joined"
input = "src"
operation = "spatial_join"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Operation);
        assert!(
            problem.message.contains("second dataset"),
            "{}",
            problem.message
        );
    }

    /// A manifest with one transform over one source, so a test only has to say
    /// what the transform is.
    fn manifest_with(transform: &str) -> String {
        format!(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "step"
input = "src"
{transform}
"#
        )
    }

    /// The parameters an operation cannot run without, and a manifest body that
    /// supplies them.
    const REQUIRED: &[(&str, &str, &str)] = &[
        ("buffer", "distance", "distance = 500.0"),
        ("simplify", "epsilon", "epsilon = 0.01"),
        ("filter", "field", "field = \"type\"\nequals = \"road\""),
        ("filter", "equals", "field = \"type\"\nequals = \"road\""),
        (
            "expression",
            "expressions",
            "expressions = { acres = \"$area / 4047\" }",
        ),
        (
            "clip",
            "min_x",
            "min_x = 0.0\nmin_y = 0.0\nmax_x = 1.0\nmax_y = 1.0",
        ),
    ];

    #[test]
    fn test_a_missing_required_parameter_is_rejected() {
        for (op, param, _) in REQUIRED {
            let problem =
                validate_manifest(&manifest_with(&format!("operation = \"{op}\""))).unwrap_err();
            assert_eq!(problem.kind, ProblemKind::Operation, "{op}");
            assert_eq!(problem.status(), StatusCode::UNPROCESSABLE_ENTITY);
            // the message names the transform, the operation and the parameter
            for part in ["step", op, param] {
                assert!(problem.message.contains(part), "{}", problem.message);
            }
        }
    }

    #[test]
    fn test_the_same_manifest_passes_once_the_parameter_is_there() {
        for (op, _, params) in REQUIRED {
            let manifest = manifest_with(&format!("operation = \"{op}\"\n{params}"));
            assert!(
                validate_manifest(&manifest).is_ok(),
                "{op}: {:?}",
                validate_manifest(&manifest).unwrap_err().message
            );
        }
    }

    /// The rejection tells the caller what the parameter is for, because a model
    /// reads it and retries.
    #[test]
    fn test_the_rejection_says_what_the_parameter_is_for() {
        let problem = validate_manifest(&manifest_with("operation = \"buffer\"")).unwrap_err();
        assert_eq!(
            problem.message,
            "transform 'step' uses operation 'buffer' which cannot run: \
             missing required parameter 'distance' (Buffer distance in meters, \
             negative to shrink a polygon)"
        );
    }

    #[test]
    fn test_clip_takes_the_whole_box_or_none_of_it() {
        let whole = manifest_with(
            "operation = \"clip\"\nmin_x = 0.0\nmin_y = 0.0\nmax_x = 1.0\nmax_y = 1.0",
        );
        assert!(validate_manifest(&whole).is_ok());

        let partial = manifest_with("operation = \"clip\"\nmin_x = 0.0\nmin_y = 0.0\nmax_x = 1.0");
        let problem = validate_manifest(&partial).unwrap_err();
        assert!(problem.message.contains("max_y"), "{}", problem.message);
        // and it does not complain about the edges that are there
        assert!(!problem.message.contains("min_x"), "{}", problem.message);
    }

    #[test]
    fn test_schema_map_that_changes_nothing_is_rejected() {
        let problem = validate_manifest(&manifest_with("operation = \"schema_map\"")).unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Operation);
        for part in ["schema_map", "rename", "drop", "add"] {
            assert!(problem.message.contains(part), "{}", problem.message);
        }

        // any one of the three is enough
        for params in [
            "rename = { old = \"new\" }",
            "drop = [\"old\"]",
            "add = { source = \"census\" }",
        ] {
            let manifest = manifest_with(&format!("operation = \"schema_map\"\n{params}"));
            assert!(validate_manifest(&manifest).is_ok(), "{params}");
        }
    }

    /// Optional by decision: reproject can autodetect the source CRS, and a
    /// dissolve with no group unions everything.
    #[test]
    fn test_optional_parameters_stay_optional() {
        assert!(validate_manifest(&manifest_with("operation = \"dissolve\"")).is_ok());
        let manifest = manifest_with("operation = \"reproject\"\nto_crs = \"EPSG:3857\"");
        assert!(validate_manifest(&manifest).is_ok());
        let problem = validate_manifest(&manifest_with("operation = \"reproject\"")).unwrap_err();
        assert!(problem.message.contains("to_crs"), "{}", problem.message);
    }

    #[test]
    fn test_unknown_source_format_is_a_format_problem() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "raster"
format = "geotiff"
path = "a.tif"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Format);
        assert!(problem.message.contains("geotiff"), "{}", problem.message);
    }

    #[test]
    fn test_unknown_sink_format_is_a_format_problem() {
        let problem = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[sink]]
name = "out"
input = "src"
format = "kml"
path = "out.kml"
"#,
        )
        .unwrap_err();
        assert_eq!(problem.kind, ProblemKind::Format);
        assert!(problem.message.contains("kml"), "{}", problem.message);
    }

    #[test]
    fn test_validation_touches_no_files() {
        // the manifest names paths that do not exist, and validating is still fine
        let plan = validate_manifest(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "/nonexistent/nope.geojson"

[[sink]]
name = "out"
input = "src"
format = "shp"
path = "/nonexistent/out.shp"
"#,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert!(!std::path::Path::new("/nonexistent/out.shp").exists());
    }
}
