//! Parallel DAG scheduler — independent waves run concurrently.

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

use crate::dag::{Dag, Node};
use crate::feature::FeatureCollection;
use crate::lineage::LineageTracker;
use crate::manifest::{Sink, Transform};
use crate::pipeline::{
    ExecutionFailure, ExecutionReport, FailedStep, PipelineError, SinkWriter, SourceReader,
    StepResult, TransformOp,
};
use crate::quality::QualityRule;
use crate::routing::EngineRouter;

/// Parallel pipeline executor. [`crate::pipeline::Pipeline::execute`] uses this.
pub struct ParallelScheduler;

impl ParallelScheduler {
    /// Execute one run as independent waves. Sources in a wave load together,
    /// then engine-resident transforms run in order, then local transforms
    /// and sinks in that wave run together.
    pub fn execute(
        dag: &Dag,
        order: &[&Node],
        reader: &dyn SourceReader,
        transforms: &HashMap<String, Box<dyn TransformOp>>,
        writer: &dyn SinkWriter,
        check_quality: bool,
    ) -> Result<(ExecutionReport, LineageTracker), Box<ExecutionFailure>> {
        let waves = dag
            .execution_waves()
            .map_err(|e| Box::new(ExecutionFailure::before_start(e.into())))?;

        let mut run = Run {
            order,
            reader,
            transforms,
            writer,
            check_quality,
            data: HashMap::new(),
            report: ExecutionReport::default(),
            router: EngineRouter::new(order, transforms),
            lineage: LineageTracker::new(),
        };
        for wave in waves {
            run.wave(wave)?;
        }

        Ok((run.report, run.lineage))
    }
}

struct Run<'a> {
    order: &'a [&'a Node],
    reader: &'a dyn SourceReader,
    transforms: &'a HashMap<String, Box<dyn TransformOp>>,
    writer: &'a dyn SinkWriter,
    check_quality: bool,
    data: HashMap<String, FeatureCollection>,
    report: ExecutionReport,
    router: EngineRouter,
    lineage: LineageTracker,
}

impl Run<'_> {
    fn wave(&mut self, wave: Vec<&Node>) -> Result<(), Box<ExecutionFailure>> {
        let mut sources = Vec::new();
        let mut resident = Vec::new();
        let mut local = Vec::new();
        for node in wave {
            match node {
                Node::Source(source) => sources.push(source),
                Node::Transform(transform) if self.router.is_resident(&transform.name) => {
                    resident.push(transform);
                }
                Node::Transform(transform) => local.push(LocalWork::Transform(transform)),
                Node::Sink(sink) => local.push(LocalWork::Sink(sink)),
            }
        }

        let loaded = sources
            .par_iter()
            .map(|source| {
                self.reader
                    .read_source(source)
                    .map(|fc| (source.name.clone(), fc))
            })
            .collect::<Vec<_>>();

        let mut source_ok = Vec::new();
        for result in loaded {
            match result {
                Ok(pair) => source_ok.push(pair),
                Err(error) => {
                    let name = match &error {
                        PipelineError::Source { name, .. } => name.clone(),
                        _ => "source".into(),
                    };
                    return Err(fail(self.order, self.report.steps.clone(), &name, error));
                }
            }
        }
        source_ok.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, fc) in source_ok {
            let source = sources
                .iter()
                .find(|s| s.name == name)
                .expect("loaded source is in this wave");
            let resident_src = self
                .router
                .admit_source(self.order, source, &fc)
                .map_err(|error| fail(self.order, self.report.steps.clone(), &name, error))?;
            self.report.record_step(&name, fc.len());
            if !resident_src || self.router.feeds_off_engine(self.order, &name) {
                self.data.insert(name, fc);
            }
        }

        for transform in resident {
            if let Err(error) = pull_resident(
                transform,
                self.order,
                &mut self.data,
                &mut self.report,
                &self.router,
            ) {
                return Err(fail(
                    self.order,
                    self.report.steps.clone(),
                    &transform.name,
                    error,
                ));
            }
        }

        let local_results: Vec<LocalOutcome> = local
            .par_iter()
            .map(|work| {
                run_local(
                    work,
                    &self.data,
                    self.transforms,
                    self.writer,
                    self.check_quality,
                )
            })
            .collect();

        if let Some(failed_name) = first_failure_name(self.order, &local_results) {
            for result in &local_results {
                if result.error.is_none() {
                    self.report.record_step(&result.name, result.feature_count);
                }
            }
            let error = local_results
                .into_iter()
                .find(|r| r.name == failed_name)
                .and_then(|r| r.error)
                .expect("picked a failure");
            return Err(fail(
                self.order,
                self.report.steps.clone(),
                &failed_name,
                error,
            ));
        }

        let mut ok: Vec<LocalOutcome> = local_results;
        ok.sort_by(|a, b| a.name.cmp(&b.name));
        for result in ok {
            self.report.record_step(&result.name, result.feature_count);
            if let Some(input) = result.lineage_from {
                self.lineage
                    .record_passthrough(&result.name, &input, result.feature_count);
            }
            if let Some(fc) = result.data {
                self.data.insert(result.name, fc);
            }
        }

        Ok(())
    }
}

enum LocalWork<'a> {
    Transform(&'a Transform),
    Sink(&'a Sink),
}

struct LocalOutcome {
    name: String,
    feature_count: usize,
    data: Option<FeatureCollection>,
    lineage_from: Option<String>,
    error: Option<PipelineError>,
}

fn run_local(
    work: &LocalWork<'_>,
    data: &HashMap<String, FeatureCollection>,
    transforms: &HashMap<String, Box<dyn TransformOp>>,
    writer: &dyn SinkWriter,
    check_quality: bool,
) -> LocalOutcome {
    match work {
        LocalWork::Transform(transform) => {
            apply_transform(transform, data, transforms, check_quality)
        }
        LocalWork::Sink(sink) => write_sink(sink, data, writer),
    }
}

fn apply_transform(
    transform: &Transform,
    data: &HashMap<String, FeatureCollection>,
    transforms: &HashMap<String, Box<dyn TransformOp>>,
    check_quality: bool,
) -> LocalOutcome {
    let input_data = match data.get(&transform.input) {
        Some(fc) => fc,
        None => {
            return LocalOutcome {
                name: transform.name.clone(),
                feature_count: 0,
                data: None,
                lineage_from: None,
                error: Some(PipelineError::Transform {
                    name: transform.name.clone(),
                    message: format!("input '{}' not available", transform.input),
                }),
            };
        }
    };
    let op = match transforms.get(&transform.operation) {
        Some(op) => op,
        None => {
            return LocalOutcome {
                name: transform.name.clone(),
                feature_count: 0,
                data: None,
                lineage_from: None,
                error: Some(PipelineError::Transform {
                    name: transform.name.clone(),
                    message: format!("unknown operation '{}'", transform.operation),
                }),
            };
        }
    };
    match op.apply(input_data, &transform.params) {
        Ok(result) => {
            if check_quality {
                let failures: Vec<_> =
                    crate::quality::check_quality(&result, &[QualityRule::GeometryValid])
                        .into_iter()
                        .filter(|r| !r.passed)
                        .collect();
                if let Some(fail) = failures.first() {
                    return LocalOutcome {
                        name: transform.name.clone(),
                        feature_count: 0,
                        data: None,
                        lineage_from: None,
                        error: Some(PipelineError::Transform {
                            name: transform.name.clone(),
                            message: fail.message.clone(),
                        }),
                    };
                }
            }
            LocalOutcome {
                name: transform.name.clone(),
                feature_count: result.len(),
                data: Some(result),
                lineage_from: Some(transform.input.clone()),
                error: None,
            }
        }
        Err(error) => LocalOutcome {
            name: transform.name.clone(),
            feature_count: 0,
            data: None,
            lineage_from: None,
            error: Some(error),
        },
    }
}

fn write_sink(
    sink: &Sink,
    data: &HashMap<String, FeatureCollection>,
    writer: &dyn SinkWriter,
) -> LocalOutcome {
    let input_data = match data.get(&sink.input) {
        Some(fc) => fc,
        None => {
            return LocalOutcome {
                name: sink.name.clone(),
                feature_count: 0,
                data: None,
                lineage_from: None,
                error: Some(PipelineError::Sink {
                    name: sink.name.clone(),
                    message: format!("input '{}' not available", sink.input),
                }),
            };
        }
    };
    match writer.write_sink(input_data, sink) {
        Ok(()) => LocalOutcome {
            name: sink.name.clone(),
            feature_count: input_data.len(),
            data: None,
            lineage_from: None,
            error: None,
        },
        Err(error) => LocalOutcome {
            name: sink.name.clone(),
            feature_count: 0,
            data: None,
            lineage_from: None,
            error: Some(error),
        },
    }
}

fn pull_resident(
    transform: &Transform,
    order: &[&Node],
    data: &mut HashMap<String, FeatureCollection>,
    report: &mut ExecutionReport,
    router: &EngineRouter,
) -> Result<(), PipelineError> {
    let wanted = router.feeds_off_engine(order, &transform.name);
    let (count, materialized) = router.pull(&transform.name, wanted)?;
    report.record_step(&transform.name, count);
    if let Some(fc) = materialized {
        data.insert(transform.name.clone(), fc);
    }
    Ok(())
}

fn first_failure_name(order: &[&Node], results: &[LocalOutcome]) -> Option<String> {
    let failed: HashSet<&str> = results
        .iter()
        .filter(|r| r.error.is_some())
        .map(|r| r.name.as_str())
        .collect();
    order
        .iter()
        .map(|node| node.name())
        .find(|name| failed.contains(name))
        .map(str::to_string)
}

fn fail(
    order: &[&Node],
    completed: Vec<StepResult>,
    failed: &str,
    error: PipelineError,
) -> Box<ExecutionFailure> {
    let done: HashSet<String> = completed.iter().map(|s| s.name.clone()).collect();
    let not_run = order
        .iter()
        .map(|n| n.name().to_string())
        .filter(|name| name != failed && !done.contains(name))
        .collect();
    Box::new(ExecutionFailure {
        completed,
        failed: Some(FailedStep {
            name: failed.to_string(),
            message: error.to_string(),
        }),
        not_run,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{Feature, Value};
    use crate::geometry::{FeatureGeometry, Point};
    use crate::manifest::{Manifest, Source};
    use crate::pipeline::Pipeline;

    struct MockReader;
    impl SourceReader for MockReader {
        fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
            Ok(FeatureCollection::new(
                vec![Feature {
                    geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
                    properties: HashMap::from([("id".into(), Value::Integer(1))]),
                }],
                Some("EPSG:4326".into()),
            ))
        }
    }

    struct Passthrough;
    impl TransformOp for Passthrough {
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
    fn test_independent_branches_run() {
        let toml = r#"
[project]
name = "branches"

[[source]]
name = "left"
format = "geojson"
path = "a.geojson"

[[source]]
name = "right"
format = "geojson"
path = "b.geojson"

[[transform]]
name = "left_out"
input = "left"
operation = "passthrough"

[[transform]]
name = "right_out"
input = "right"
operation = "passthrough"

[[sink]]
name = "left_sink"
input = "left_out"
format = "geojson"
path = "a.out"

[[sink]]
name = "right_sink"
input = "right_out"
format = "geojson"
path = "b.out"
"#;
        let pipeline = Pipeline::new(Manifest::from_toml(toml).unwrap()).unwrap();
        let mut transforms: HashMap<String, Box<dyn TransformOp>> = HashMap::new();
        transforms.insert("passthrough".into(), Box::new(Passthrough));
        let report = pipeline
            .execute(&MockReader, &transforms, &MockWriter)
            .unwrap();
        let names: HashSet<&str> = report.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            HashSet::from([
                "left",
                "right",
                "left_out",
                "right_out",
                "left_sink",
                "right_sink"
            ])
        );
        assert!(report.steps.iter().all(|s| s.feature_count == 1));
    }
}
