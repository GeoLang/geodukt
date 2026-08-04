//! Pipeline execution — runs the DAG in topological order.

use std::collections::HashMap;
use thiserror::Error;

use crate::dag::{Dag, DagError, Node};
use crate::feature::FeatureCollection;
use crate::manifest::{Manifest, Sink, Source};
use crate::routing::EngineRouter;

/// Errors during pipeline execution.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("DAG error: {0}")]
    Dag(#[from] DagError),
    #[error("source error for '{name}': {message}")]
    Source { name: String, message: String },
    #[error("transform error for '{name}': {message}")]
    Transform { name: String, message: String },
    #[error("sink error for '{name}': {message}")]
    Sink { name: String, message: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Trait for reading feature data from a source.
pub trait SourceReader {
    fn read_source(&self, source: &Source) -> Result<FeatureCollection, PipelineError>;
}

/// Trait for applying a spatial transform.
pub trait TransformOp {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError>;
}

/// Trait for writing feature data to a sink.
pub trait SinkWriter {
    fn write_sink(&self, data: &FeatureCollection, sink: &Sink) -> Result<(), PipelineError>;
}

/// Pipeline executor — runs the DAG with pluggable source/transform/sink implementations.
pub struct Pipeline {
    dag: Dag,
    manifest: Manifest,
}

impl Pipeline {
    /// Create a pipeline from a manifest.
    pub fn new(manifest: Manifest) -> Result<Self, PipelineError> {
        let dag = Dag::from_manifest(&manifest)?;
        Ok(Self { dag, manifest })
    }

    /// The nodes in execution order, for inspecting a plan without running it.
    pub fn plan(&self) -> Result<Vec<&Node>, PipelineError> {
        Ok(self.dag.topological_order()?)
    }

    /// Validate the pipeline DAG without executing.
    pub fn validate(&self) -> Result<Vec<String>, PipelineError> {
        Ok(self.plan()?.iter().map(|n| n.name().to_string()).collect())
    }

    /// Execute the pipeline with the given source/transform/sink implementations.
    ///
    /// The head of the pipeline the engine can run goes onto a geoplumb graph
    /// and comes back at the first node that cannot, see [`crate::routing`].
    ///
    /// On failure the steps that already ran come back with the error, so a
    /// caller can record how far the run got.
    pub fn execute(
        &self,
        reader: &dyn SourceReader,
        transforms: &HashMap<String, Box<dyn TransformOp>>,
        writer: &dyn SinkWriter,
    ) -> Result<ExecutionReport, Box<ExecutionFailure>> {
        let order = self
            .dag
            .topological_order()
            .map_err(|e| Box::new(ExecutionFailure::before_start(e.into())))?;

        let io = Io {
            reader,
            transforms,
            writer,
        };
        let mut state = RunState {
            order: &order,
            data: HashMap::new(),
            report: ExecutionReport::default(),
            router: EngineRouter::new(&order, transforms),
        };

        for (i, node) in order.iter().enumerate() {
            if let Err(error) = run_node(node, &io, &mut state) {
                return Err(Box::new(ExecutionFailure {
                    completed: state.report.steps,
                    failed: Some(FailedStep {
                        name: node.name().to_string(),
                        message: error.to_string(),
                    }),
                    not_run: order[i + 1..]
                        .iter()
                        .map(|n| n.name().to_string())
                        .collect(),
                    error,
                }));
            }
        }

        Ok(state.report)
    }

    /// Get the manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// The implementations one run was handed.
struct Io<'a> {
    reader: &'a dyn SourceReader,
    transforms: &'a HashMap<String, Box<dyn TransformOp>>,
    writer: &'a dyn SinkWriter,
}

/// What a run carries from node to node.
struct RunState<'a> {
    order: &'a [&'a Node],
    data: HashMap<String, FeatureCollection>,
    report: ExecutionReport,
    router: EngineRouter,
}

fn run_node(node: &Node, io: &Io<'_>, state: &mut RunState<'_>) -> Result<(), PipelineError> {
    match node {
        Node::Source(source) => {
            let fc = io.reader.read_source(source)?;
            // a source on the engine holds the same features there, so its
            // count is the collection's either way and the collection itself
            // is kept only for whatever still reads it off the engine
            let resident = state.router.admit_source(state.order, source, &fc)?;
            state.report.record_step(&source.name, fc.len());
            if !resident || state.router.feeds_off_engine(state.order, &source.name) {
                state.data.insert(source.name.clone(), fc);
            }
        }
        Node::Transform(transform) if state.router.is_resident(&transform.name) => {
            let wanted = state.router.feeds_off_engine(state.order, &transform.name);
            let (count, materialized) = state.router.pull(&transform.name, wanted)?;
            state.report.record_step(&transform.name, count);
            if let Some(fc) = materialized {
                state.data.insert(transform.name.clone(), fc);
            }
        }
        Node::Transform(transform) => {
            let input_data =
                state
                    .data
                    .get(&transform.input)
                    .ok_or_else(|| PipelineError::Transform {
                        name: transform.name.clone(),
                        message: format!("input '{}' not available", transform.input),
                    })?;
            let op = io.transforms.get(&transform.operation).ok_or_else(|| {
                PipelineError::Transform {
                    name: transform.name.clone(),
                    message: format!("unknown operation '{}'", transform.operation),
                }
            })?;
            let result = op.apply(input_data, &transform.params)?;
            state.report.record_step(&transform.name, result.len());
            state.data.insert(transform.name.clone(), result);
        }
        Node::Sink(sink) => {
            let input_data = state
                .data
                .get(&sink.input)
                .ok_or_else(|| PipelineError::Sink {
                    name: sink.name.clone(),
                    message: format!("input '{}' not available", sink.input),
                })?;
            io.writer.write_sink(input_data, sink)?;
            state.report.record_step(&sink.name, input_data.len());
        }
    }
    Ok(())
}

/// Report from a pipeline execution.
#[derive(Debug, Default)]
pub struct ExecutionReport {
    pub steps: Vec<StepResult>,
}

/// Result of a single pipeline step.
#[derive(Debug)]
pub struct StepResult {
    pub name: String,
    pub feature_count: usize,
}

/// A failed execution, with the progress made before the failure.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct ExecutionFailure {
    /// Steps that finished before the failure, in execution order.
    pub completed: Vec<StepResult>,
    /// The step that failed, absent when the DAG could not be ordered at all.
    pub failed: Option<FailedStep>,
    /// Steps the run never reached, in execution order.
    pub not_run: Vec<String>,
    #[source]
    pub error: PipelineError,
}

/// The step a run died on, and why.
#[derive(Debug)]
pub struct FailedStep {
    pub name: String,
    pub message: String,
}

impl ExecutionFailure {
    fn before_start(error: PipelineError) -> Self {
        Self {
            completed: Vec::new(),
            failed: None,
            not_run: Vec::new(),
            error,
        }
    }

    /// The underlying error, for callers that do not care about the progress.
    pub fn error(&self) -> &PipelineError {
        &self.error
    }
}

impl From<ExecutionFailure> for PipelineError {
    fn from(failure: ExecutionFailure) -> Self {
        failure.error
    }
}

impl ExecutionReport {
    fn record_step(&mut self, name: &str, feature_count: usize) {
        self.steps.push(StepResult {
            name: name.to_string(),
            feature_count,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{Feature, FeatureCollection, Value};
    use crate::geometry::{FeatureGeometry, Point};

    struct MockReader;
    impl SourceReader for MockReader {
        fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
            let features = vec![Feature {
                geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
                properties: HashMap::from([("id".into(), Value::Integer(1))]),
            }];
            Ok(FeatureCollection::new(features, Some("EPSG:4326".into())))
        }
    }

    struct PassthroughTransform;
    impl TransformOp for PassthroughTransform {
        fn apply(
            &self,
            input: &FeatureCollection,
            _params: &HashMap<String, toml::Value>,
        ) -> Result<FeatureCollection, PipelineError> {
            Ok(input.clone())
        }
    }

    struct MockWriter;
    impl SinkWriter for MockWriter {
        fn write_sink(&self, _data: &FeatureCollection, _sink: &Sink) -> Result<(), PipelineError> {
            Ok(())
        }
    }

    #[test]
    fn test_pipeline_execute() {
        let toml = r#"
[project]
name = "test"

[[source]]
name = "input"
format = "geojson"
path = "data.geojson"

[[transform]]
name = "processed"
input = "input"
operation = "passthrough"

[[sink]]
name = "output"
input = "processed"
format = "geojson"
path = "out.geojson"
"#;
        let manifest = Manifest::from_toml(toml).unwrap();
        let pipeline = Pipeline::new(manifest).unwrap();

        let mut transforms: HashMap<String, Box<dyn TransformOp>> = HashMap::new();
        transforms.insert("passthrough".into(), Box::new(PassthroughTransform));

        let report = pipeline
            .execute(&MockReader, &transforms, &MockWriter)
            .unwrap();
        assert_eq!(report.steps.len(), 3);
        assert_eq!(report.steps[0].name, "input");
        assert_eq!(report.steps[0].feature_count, 1);
    }

    #[test]
    fn test_execute_failure_carries_progress() {
        struct FailingTransform;
        impl TransformOp for FailingTransform {
            fn apply(
                &self,
                _input: &FeatureCollection,
                _params: &HashMap<String, toml::Value>,
            ) -> Result<FeatureCollection, PipelineError> {
                Err(PipelineError::Transform {
                    name: "processed".into(),
                    message: "boom".into(),
                })
            }
        }

        let toml = r#"
[project]
name = "test"

[[source]]
name = "input"
format = "geojson"
path = "data.geojson"

[[transform]]
name = "processed"
input = "input"
operation = "explode"

[[sink]]
name = "output"
input = "processed"
format = "geojson"
path = "out.geojson"
"#;
        let manifest = Manifest::from_toml(toml).unwrap();
        let pipeline = Pipeline::new(manifest).unwrap();

        let mut transforms: HashMap<String, Box<dyn TransformOp>> = HashMap::new();
        transforms.insert("explode".into(), Box::new(FailingTransform));

        let failure = pipeline
            .execute(&MockReader, &transforms, &MockWriter)
            .unwrap_err();

        assert_eq!(failure.completed.len(), 1);
        assert_eq!(failure.completed[0].name, "input");
        let failed = failure.failed.as_ref().unwrap();
        assert_eq!(failed.name, "processed");
        assert!(failed.message.contains("boom"), "{}", failed.message);
        assert_eq!(failure.not_run, vec!["output"]);
    }

    #[test]
    fn test_pipeline_validate() {
        let toml = r#"
[project]
name = "validate-test"

[[source]]
name = "src"
format = "csv"
path = "data.csv"

[[sink]]
name = "out"
input = "src"
format = "geojson"
path = "out.geojson"
"#;
        let manifest = Manifest::from_toml(toml).unwrap();
        let pipeline = Pipeline::new(manifest).unwrap();
        let order = pipeline.validate().unwrap();
        assert_eq!(order, vec!["src", "out"]);
    }
}
