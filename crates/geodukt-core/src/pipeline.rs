//! Pipeline execution — runs the DAG in topological order.

use std::collections::HashMap;
use thiserror::Error;

use crate::dag::{Dag, DagError, Node};
use crate::feature::FeatureCollection;
use crate::manifest::{Manifest, Sink, Source};

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
    pub fn execute(
        &self,
        reader: &dyn SourceReader,
        transforms: &HashMap<String, Box<dyn TransformOp>>,
        writer: &dyn SinkWriter,
    ) -> Result<ExecutionReport, PipelineError> {
        let order = self.dag.topological_order()?;
        let mut data: HashMap<String, FeatureCollection> = HashMap::new();
        let mut report = ExecutionReport::default();

        for node in order {
            match node {
                Node::Source(source) => {
                    let fc = reader.read_source(source)?;
                    report.record_step(&source.name, fc.len());
                    data.insert(source.name.clone(), fc);
                }
                Node::Transform(transform) => {
                    let input_data =
                        data.get(&transform.input)
                            .ok_or_else(|| PipelineError::Transform {
                                name: transform.name.clone(),
                                message: format!("input '{}' not available", transform.input),
                            })?;
                    let op = transforms.get(&transform.operation).ok_or_else(|| {
                        PipelineError::Transform {
                            name: transform.name.clone(),
                            message: format!("unknown operation '{}'", transform.operation),
                        }
                    })?;
                    let result = op.apply(input_data, &transform.params)?;
                    report.record_step(&transform.name, result.len());
                    data.insert(transform.name.clone(), result);
                }
                Node::Sink(sink) => {
                    let input_data = data.get(&sink.input).ok_or_else(|| PipelineError::Sink {
                        name: sink.name.clone(),
                        message: format!("input '{}' not available", sink.input),
                    })?;
                    writer.write_sink(input_data, sink)?;
                    report.record_step(&sink.name, input_data.len());
                }
            }
        }

        Ok(report)
    }

    /// Get the manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
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
    use geo::point;

    struct MockReader;
    impl SourceReader for MockReader {
        fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
            let features = vec![Feature {
                geometry: geo::Geometry::Point(point!(x: 1.0, y: 2.0)),
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
