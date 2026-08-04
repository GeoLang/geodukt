//! Streaming mode — process features one-at-a-time for memory-bounded operation.

use crate::feature::Feature;
use crate::pipeline::PipelineError;

/// A streaming feature processor that handles features one at a time.
pub trait StreamProcessor {
    /// Process a single feature. Returns None to drop the feature, Some to emit.
    fn process(&mut self, feature: Feature) -> Result<Option<Feature>, PipelineError>;

    /// Called when all features have been processed. Returns any buffered output.
    fn flush(&mut self) -> Result<Vec<Feature>, PipelineError> {
        Ok(Vec::new())
    }
}

/// A streaming source that yields features one at a time.
pub trait StreamSource {
    /// Get the next feature, or None if exhausted.
    fn next_feature(&mut self) -> Result<Option<Feature>, PipelineError>;
}

/// A streaming sink that accepts features one at a time.
pub trait StreamSink {
    /// Write a single feature.
    fn write_feature(&mut self, feature: &Feature) -> Result<(), PipelineError>;

    /// Finalize the output (flush buffers, close files).
    fn finalize(&mut self) -> Result<(), PipelineError>;
}

/// Execute a streaming pipeline: source → processors → sink.
pub fn execute_streaming(
    source: &mut dyn StreamSource,
    processors: &mut [Box<dyn StreamProcessor>],
    sink: &mut dyn StreamSink,
) -> Result<StreamReport, PipelineError> {
    let mut input_count = 0usize;
    let mut output_count = 0usize;

    while let Some(feature) = source.next_feature()? {
        input_count += 1;

        let mut current = Some(feature);
        for proc in processors.iter_mut() {
            current = match current {
                Some(f) => proc.process(f)?,
                None => break,
            };
        }

        if let Some(f) = current {
            sink.write_feature(&f)?;
            output_count += 1;
        }
    }

    // Flush all processors
    for proc in processors.iter_mut() {
        let flushed = proc.flush()?;
        for f in flushed {
            sink.write_feature(&f)?;
            output_count += 1;
        }
    }

    sink.finalize()?;

    Ok(StreamReport {
        input_count,
        output_count,
    })
}

/// Report from streaming execution.
#[derive(Debug)]
pub struct StreamReport {
    pub input_count: usize,
    pub output_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Value;
    use crate::geometry::{FeatureGeometry, Point};
    use std::collections::HashMap;

    struct VecSource {
        features: Vec<Feature>,
        idx: usize,
    }

    impl StreamSource for VecSource {
        fn next_feature(&mut self) -> Result<Option<Feature>, PipelineError> {
            if self.idx < self.features.len() {
                let f = self.features[self.idx].clone();
                self.idx += 1;
                Ok(Some(f))
            } else {
                Ok(None)
            }
        }
    }

    struct FilterProc {
        field: String,
        value: Value,
    }

    impl StreamProcessor for FilterProc {
        fn process(&mut self, feature: Feature) -> Result<Option<Feature>, PipelineError> {
            if feature.properties.get(&self.field) == Some(&self.value) {
                Ok(Some(feature))
            } else {
                Ok(None)
            }
        }
    }

    struct CollectSink {
        collected: Vec<Feature>,
    }

    impl StreamSink for CollectSink {
        fn write_feature(&mut self, feature: &Feature) -> Result<(), PipelineError> {
            self.collected.push(feature.clone());
            Ok(())
        }
        fn finalize(&mut self) -> Result<(), PipelineError> {
            Ok(())
        }
    }

    #[test]
    fn test_streaming_pipeline() {
        let features = vec![
            Feature {
                geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
                properties: HashMap::from([("keep".into(), Value::Bool(true))]),
            },
            Feature {
                geometry: FeatureGeometry::Point(Point::new(1.0, 1.0)),
                properties: HashMap::from([("keep".into(), Value::Bool(false))]),
            },
        ];

        let mut source = VecSource { features, idx: 0 };
        let mut processors: Vec<Box<dyn StreamProcessor>> = vec![Box::new(FilterProc {
            field: "keep".into(),
            value: Value::Bool(true),
        })];
        let mut sink = CollectSink {
            collected: Vec::new(),
        };

        let report = execute_streaming(&mut source, &mut processors, &mut sink).unwrap();
        assert_eq!(report.input_count, 2);
        assert_eq!(report.output_count, 1);
        assert_eq!(sink.collected.len(), 1);
    }
}
