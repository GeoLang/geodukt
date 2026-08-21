//! Lineage tracking — records provenance of features through the pipeline.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Records which source features contributed to each output feature.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LineageTracker {
    /// Map from output feature ID to source feature IDs that contributed.
    pub records: Vec<LineageRecord>,
}

/// A single lineage record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageRecord {
    pub output_node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_feature_idx: Option<usize>,
    pub source_nodes: Vec<SourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_count: Option<usize>,
}

/// Reference to a source feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_idx: Option<usize>,
}

impl LineageTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that an output feature was derived from specific source features.
    pub fn record(&mut self, output_node: &str, output_idx: usize, sources: Vec<SourceRef>) {
        self.records.push(LineageRecord {
            output_node: output_node.to_string(),
            output_feature_idx: Some(output_idx),
            source_nodes: sources,
            output_count: None,
            input_count: None,
        });
    }

    /// Record a 1:1 passthrough (feature at index i came from same index in input).
    pub fn record_passthrough(&mut self, output_node: &str, input_node: &str, count: usize) {
        for i in 0..count {
            self.record(
                output_node,
                i,
                vec![SourceRef {
                    node: input_node.to_string(),
                    feature_idx: Some(i),
                }],
            );
        }
    }

    /// Record that an output node came from an input node, without feature indices.
    pub fn record_node(
        &mut self,
        output_node: &str,
        input_node: &str,
        output_count: usize,
        input_count: usize,
    ) {
        self.records.push(LineageRecord {
            output_node: output_node.to_string(),
            output_feature_idx: None,
            source_nodes: vec![SourceRef {
                node: input_node.to_string(),
                feature_idx: None,
            }],
            output_count: Some(output_count),
            input_count: Some(input_count),
        });
    }

    /// Get all sources for a given output feature.
    pub fn sources_for(&self, output_node: &str, feature_idx: usize) -> Vec<&SourceRef> {
        self.records
            .iter()
            .filter(|r| r.output_node == output_node && r.output_feature_idx == Some(feature_idx))
            .flat_map(|r| &r.source_nodes)
            .collect()
    }

    /// Get lineage summary as a map of node → feature count.
    pub fn summary(&self) -> HashMap<String, usize> {
        let mut map: HashMap<String, usize> = HashMap::new();
        for r in &self.records {
            *map.entry(r.output_node.clone()).or_default() += 1;
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lineage_tracking() {
        let mut tracker = LineageTracker::new();
        tracker.record_passthrough("transform_a", "source_1", 3);
        tracker.record(
            "transform_b",
            0,
            vec![
                SourceRef {
                    node: "transform_a".into(),
                    feature_idx: Some(0),
                },
                SourceRef {
                    node: "transform_a".into(),
                    feature_idx: Some(1),
                },
            ],
        );

        let sources = tracker.sources_for("transform_b", 0);
        assert_eq!(sources.len(), 2);

        let summary = tracker.summary();
        assert_eq!(summary.get("transform_a"), Some(&3));
        assert_eq!(summary.get("transform_b"), Some(&1));
    }
}
