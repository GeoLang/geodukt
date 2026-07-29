//! Parallel DAG scheduler — executes independent branches concurrently.

use rayon::prelude::*;
use std::collections::HashMap;

use crate::dag::{Dag, Node};
use crate::feature::FeatureCollection;
use crate::pipeline::{
    ExecutionReport, PipelineError, SinkWriter, SourceReader, StepResult, TransformOp,
};

/// Parallel pipeline executor — runs independent DAG branches concurrently.
pub struct ParallelScheduler;

impl ParallelScheduler {
    /// Execute pipeline with parallel source loading and independent branch execution.
    pub fn execute(
        dag: &Dag,
        reader: &(dyn SourceReader + Sync),
        transforms: &HashMap<String, Box<dyn TransformOp + Sync + Send>>,
        writer: &(dyn SinkWriter + Sync),
    ) -> Result<ExecutionReport, PipelineError> {
        let order = dag.topological_order()?;

        // Identify source nodes that can be loaded in parallel
        let sources: Vec<&Node> = order
            .iter()
            .filter(|n| matches!(n, Node::Source(_)))
            .copied()
            .collect();

        // Load sources in parallel
        let source_data: Vec<Result<(String, FeatureCollection), PipelineError>> = sources
            .par_iter()
            .map(|node| {
                if let Node::Source(source) = node {
                    let fc = reader.read_source(source)?;
                    Ok((source.name.clone(), fc))
                } else {
                    unreachable!()
                }
            })
            .collect();

        let mut data: HashMap<String, FeatureCollection> = HashMap::new();
        let mut report = ExecutionReport::default();

        for result in source_data {
            let (name, fc) = result?;
            report.steps.push(StepResult {
                name: name.clone(),
                feature_count: fc.len(),
            });
            data.insert(name, fc);
        }

        // Execute transforms and sinks in topological order (sequential for dependency)
        for node in &order {
            match node {
                Node::Source(_) => {} // Already loaded
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
                    report.steps.push(StepResult {
                        name: transform.name.clone(),
                        feature_count: result.len(),
                    });
                    data.insert(transform.name.clone(), result);
                }
                Node::Sink(sink) => {
                    let input_data = data.get(&sink.input).ok_or_else(|| PipelineError::Sink {
                        name: sink.name.clone(),
                        message: format!("input '{}' not available", sink.input),
                    })?;
                    writer.write_sink(input_data, sink)?;
                    report.steps.push(StepResult {
                        name: sink.name.clone(),
                        feature_count: input_data.len(),
                    });
                }
            }
        }

        Ok(report)
    }
}
