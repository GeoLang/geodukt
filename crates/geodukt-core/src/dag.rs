//! DAG (directed acyclic graph) construction and topological execution.

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use thiserror::Error;

use crate::manifest::{Manifest, Sink, Source, Transform};

/// Errors from DAG operations.
#[derive(Debug, Error)]
pub enum DagError {
    #[error("cycle detected in pipeline graph")]
    CycleDetected,
    #[error("unknown input reference: {0}")]
    UnknownInput(String),
    #[error("duplicate node name: {0}")]
    DuplicateName(String),
}

/// A node in the pipeline DAG.
#[derive(Debug, Clone)]
pub enum Node {
    Source(Source),
    Transform(Transform),
    Sink(Sink),
}

impl Node {
    pub fn name(&self) -> &str {
        match self {
            Node::Source(s) => &s.name,
            Node::Transform(t) => &t.name,
            Node::Sink(s) => &s.name,
        }
    }
}

/// Pipeline DAG with topological ordering.
pub struct Dag {
    graph: DiGraph<Node, ()>,
    name_to_index: HashMap<String, NodeIndex>,
}

impl Dag {
    /// Build a DAG from a manifest.
    pub fn from_manifest(manifest: &Manifest) -> Result<Self, DagError> {
        let mut graph = DiGraph::new();
        let mut name_to_index = HashMap::new();

        // Add source nodes
        for source in &manifest.source {
            if name_to_index.contains_key(&source.name) {
                return Err(DagError::DuplicateName(source.name.clone()));
            }
            let idx = graph.add_node(Node::Source(source.clone()));
            name_to_index.insert(source.name.clone(), idx);
        }

        // Add transform nodes
        for transform in &manifest.transform {
            if name_to_index.contains_key(&transform.name) {
                return Err(DagError::DuplicateName(transform.name.clone()));
            }
            let idx = graph.add_node(Node::Transform(transform.clone()));
            name_to_index.insert(transform.name.clone(), idx);
        }

        // Add sink nodes
        for sink in &manifest.sink {
            if name_to_index.contains_key(&sink.name) {
                return Err(DagError::DuplicateName(sink.name.clone()));
            }
            let idx = graph.add_node(Node::Sink(sink.clone()));
            name_to_index.insert(sink.name.clone(), idx);
        }

        // Add edges based on input references
        for transform in &manifest.transform {
            let to = name_to_index[&transform.name];
            let from = name_to_index
                .get(&transform.input)
                .ok_or_else(|| DagError::UnknownInput(transform.input.clone()))?;
            graph.add_edge(*from, to, ());
        }

        for sink in &manifest.sink {
            let to = name_to_index[&sink.name];
            let from = name_to_index
                .get(&sink.input)
                .ok_or_else(|| DagError::UnknownInput(sink.input.clone()))?;
            graph.add_edge(*from, to, ());
        }

        let dag = Self {
            graph,
            name_to_index,
        };

        // Verify no cycles
        dag.topological_order()?;

        Ok(dag)
    }

    /// Get the topological execution order.
    pub fn topological_order(&self) -> Result<Vec<&Node>, DagError> {
        let sorted = toposort(&self.graph, None).map_err(|_| DagError::CycleDetected)?;
        Ok(sorted.iter().map(|idx| &self.graph[*idx]).collect())
    }

    /// Independent waves: every node in a wave has its inputs in an earlier one.
    /// Nodes inside a wave have no edges between them and can run together.
    pub fn execution_waves(&self) -> Result<Vec<Vec<&Node>>, DagError> {
        let mut remaining: HashMap<NodeIndex, usize> =
            self.graph.node_indices().map(|idx| (idx, 0)).collect();
        for edge in self.graph.edge_indices() {
            let (_, target) = self
                .graph
                .edge_endpoints(edge)
                .expect("edge index from the same graph");
            *remaining.get_mut(&target).expect("endpoint is a node") += 1;
        }

        let mut ready: Vec<NodeIndex> = remaining
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(idx, _)| *idx)
            .collect();
        let mut waves = Vec::new();
        let mut seen = 0usize;

        while !ready.is_empty() {
            ready.sort_by_key(|idx| self.graph[*idx].name());
            waves.push(ready.iter().map(|idx| &self.graph[*idx]).collect());
            let mut next = Vec::new();
            for idx in &ready {
                seen += 1;
                for neighbor in self.graph.neighbors(*idx) {
                    let left = remaining.get_mut(&neighbor).expect("neighbor is a node");
                    *left -= 1;
                    if *left == 0 {
                        next.push(neighbor);
                    }
                }
            }
            ready = next;
        }

        if seen != self.graph.node_count() {
            return Err(DagError::CycleDetected);
        }
        Ok(waves)
    }

    /// Get a node by name.
    pub fn get_node(&self, name: &str) -> Option<&Node> {
        self.name_to_index.get(name).map(|idx| &self.graph[*idx])
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    #[test]
    fn test_dag_from_manifest() {
        let toml = r#"
[project]
name = "test"

[[source]]
name = "input"
format = "geojson"
path = "data.geojson"

[[transform]]
name = "buffered"
input = "input"
operation = "buffer"
distance = 5.0

[[sink]]
name = "output"
input = "buffered"
format = "geojson"
path = "out.geojson"
"#;
        let manifest = Manifest::from_toml(toml).unwrap();
        let dag = Dag::from_manifest(&manifest).unwrap();
        assert_eq!(dag.node_count(), 3);
        assert_eq!(dag.edge_count(), 2);

        let order = dag.topological_order().unwrap();
        // Source must come before transform, transform before sink
        let names: Vec<&str> = order.iter().map(|n| n.name()).collect();
        assert_eq!(names, vec!["input", "buffered", "output"]);
    }

    #[test]
    fn test_dag_unknown_input() {
        let toml = r#"
[project]
name = "test"

[[transform]]
name = "broken"
input = "nonexistent"
operation = "buffer"
"#;
        let manifest = Manifest::from_toml(toml).unwrap();
        let result = Dag::from_manifest(&manifest);
        assert!(result.is_err());
    }

    #[test]
    fn test_dag_duplicate_name() {
        let toml = r#"
[project]
name = "test"

[[source]]
name = "data"
format = "geojson"
path = "a.geojson"

[[source]]
name = "data"
format = "geojson"
path = "b.geojson"
"#;
        let manifest = Manifest::from_toml(toml).unwrap();
        let result = Dag::from_manifest(&manifest);
        assert!(matches!(result, Err(DagError::DuplicateName(_))));
    }

    #[test]
    fn test_execution_waves_independent_branches() {
        let toml = r#"
[project]
name = "waves"

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
        let dag = Dag::from_manifest(&Manifest::from_toml(toml).unwrap()).unwrap();
        let waves = dag.execution_waves().unwrap();
        let names: Vec<Vec<&str>> = waves
            .iter()
            .map(|wave| wave.iter().map(|n| n.name()).collect())
            .collect();
        assert_eq!(
            names,
            vec![
                vec!["left", "right"],
                vec!["left_out", "right_out"],
                vec!["left_sink", "right_sink"],
            ]
        );
    }
}
