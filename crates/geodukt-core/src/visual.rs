//! Visual pipeline builder — JSON-serializable pipeline graph for UI rendering.
//!
//! Provides a structured representation of the pipeline DAG that frontends
//! can render as a node-graph editor, including node positions, port definitions,
//! and connection metadata.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, Sink, Source, Transform};
use crate::pipeline::PipelineError;

/// A 2D position for visual layout.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// Port direction on a visual node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortDirection {
    Input,
    Output,
}

/// A connection port on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Port {
    pub id: String,
    pub direction: PortDirection,
    pub label: String,
}

/// The kind of a visual node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Source,
    Transform,
    Sink,
}

/// A node in the visual pipeline graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualNode {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    pub position: Position,
    pub ports: Vec<Port>,
    /// Node-specific parameters (operation type, format, path, etc.)
    pub params: HashMap<String, serde_json::Value>,
}

/// An edge connecting two ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualEdge {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
}

/// The complete visual pipeline graph (serializable to/from JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualPipeline {
    pub name: String,
    pub version: String,
    pub nodes: Vec<VisualNode>,
    pub edges: Vec<VisualEdge>,
}

impl VisualPipeline {
    /// Build a visual pipeline from a manifest with auto-layout.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut y_offset = 0.0;

        // Sources at x=0
        for source in &manifest.source {
            nodes.push(visual_node_from_source(
                source,
                Position {
                    x: 0.0,
                    y: y_offset,
                },
            ));
            y_offset += 120.0;
        }

        // Transforms at x=300
        y_offset = 0.0;
        for transform in &manifest.transform {
            nodes.push(visual_node_from_transform(
                transform,
                Position {
                    x: 300.0,
                    y: y_offset,
                },
            ));

            // Edge from input to this transform
            edges.push(VisualEdge {
                id: format!("edge-{}-{}", transform.input, transform.name),
                from_node: transform.input.clone(),
                from_port: "output".to_string(),
                to_node: transform.name.clone(),
                to_port: "input".to_string(),
            });
            y_offset += 120.0;
        }

        // Sinks at x=600
        y_offset = 0.0;
        for sink in &manifest.sink {
            nodes.push(visual_node_from_sink(
                sink,
                Position {
                    x: 600.0,
                    y: y_offset,
                },
            ));

            edges.push(VisualEdge {
                id: format!("edge-{}-{}", sink.input, sink.name),
                from_node: sink.input.clone(),
                from_port: "output".to_string(),
                to_node: sink.name.clone(),
                to_port: "input".to_string(),
            });
            y_offset += 120.0;
        }

        Self {
            name: manifest.project.name.clone(),
            version: manifest.project.version.clone(),
            nodes,
            edges,
        }
    }

    /// Convert back to a pipeline manifest.
    pub fn to_manifest(&self) -> Result<Manifest, PipelineError> {
        let mut sources = Vec::new();
        let mut transforms = Vec::new();
        let mut sinks = Vec::new();

        for node in &self.nodes {
            match node.kind {
                NodeKind::Source => {
                    sources.push(Source {
                        name: node.id.clone(),
                        format: param_str(&node.params, "format"),
                        path: param_str(&node.params, "path"),
                        crs: node
                            .params
                            .get("crs")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        layer: node
                            .params
                            .get("layer")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
                NodeKind::Transform => {
                    let input = self
                        .edges
                        .iter()
                        .find(|e| e.to_node == node.id)
                        .map(|e| e.from_node.clone())
                        .unwrap_or_default();

                    let mut params: HashMap<String, toml::Value> = HashMap::new();
                    for (k, v) in &node.params {
                        if k != "operation"
                            && let Some(s) = v.as_str()
                        {
                            params.insert(k.clone(), toml::Value::String(s.to_string()));
                        }
                    }

                    transforms.push(Transform {
                        name: node.id.clone(),
                        input,
                        operation: param_str(&node.params, "operation"),
                        params,
                    });
                }
                NodeKind::Sink => {
                    let input = self
                        .edges
                        .iter()
                        .find(|e| e.to_node == node.id)
                        .map(|e| e.from_node.clone())
                        .unwrap_or_default();

                    sinks.push(Sink {
                        name: node.id.clone(),
                        input,
                        format: param_str(&node.params, "format"),
                        path: param_str(&node.params, "path"),
                        layer: node
                            .params
                            .get("layer")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
            }
        }

        Ok(Manifest {
            project: crate::manifest::Project {
                name: self.name.clone(),
                version: self.version.clone(),
                incremental: false,
                lineage: false,
                quality: false,
            },
            source: sources,
            transform: transforms,
            sink: sinks,
        })
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize from JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

fn param_str(params: &HashMap<String, serde_json::Value>, key: &str) -> String {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn visual_node_from_source(source: &Source, position: Position) -> VisualNode {
    VisualNode {
        id: source.name.clone(),
        kind: NodeKind::Source,
        label: source.name.clone(),
        position,
        ports: vec![Port {
            id: "output".to_string(),
            direction: PortDirection::Output,
            label: "data".to_string(),
        }],
        params: HashMap::from([
            ("format".to_string(), serde_json::json!(source.format)),
            ("path".to_string(), serde_json::json!(source.path)),
            (
                "crs".to_string(),
                serde_json::json!(source.crs.as_deref().unwrap_or("")),
            ),
        ]),
    }
}

fn visual_node_from_transform(transform: &Transform, position: Position) -> VisualNode {
    let mut params: HashMap<String, serde_json::Value> = HashMap::new();
    params.insert(
        "operation".to_string(),
        serde_json::json!(transform.operation),
    );
    for (k, v) in &transform.params {
        params.insert(k.clone(), serde_json::json!(v.to_string()));
    }

    VisualNode {
        id: transform.name.clone(),
        kind: NodeKind::Transform,
        label: transform.name.clone(),
        position,
        ports: vec![
            Port {
                id: "input".to_string(),
                direction: PortDirection::Input,
                label: "in".to_string(),
            },
            Port {
                id: "output".to_string(),
                direction: PortDirection::Output,
                label: "out".to_string(),
            },
        ],
        params,
    }
}

fn visual_node_from_sink(sink: &Sink, position: Position) -> VisualNode {
    VisualNode {
        id: sink.name.clone(),
        kind: NodeKind::Sink,
        label: sink.name.clone(),
        position,
        ports: vec![Port {
            id: "input".to_string(),
            direction: PortDirection::Input,
            label: "data".to_string(),
        }],
        params: HashMap::from([
            ("format".to_string(), serde_json::json!(sink.format)),
            ("path".to_string(), serde_json::json!(sink.path)),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest() -> Manifest {
        Manifest::from_toml(
            r#"
[project]
name = "test-pipeline"
version = "1.0.0"

[[source]]
name = "roads"
format = "geojson"
path = "data/roads.geojson"
crs = "EPSG:4326"

[[transform]]
name = "reproject"
input = "roads"
operation = "reproject"
to_crs = "EPSG:3857"

[[sink]]
name = "output"
input = "reproject"
format = "geopackage"
path = "output/roads.gpkg"
"#,
        )
        .unwrap()
    }

    #[test]
    fn test_visual_from_manifest() {
        let manifest = test_manifest();
        let visual = VisualPipeline::from_manifest(&manifest);

        assert_eq!(visual.nodes.len(), 3);
        assert_eq!(visual.edges.len(), 2);
        assert_eq!(visual.name, "test-pipeline");
    }

    #[test]
    fn test_visual_roundtrip() {
        let manifest = test_manifest();
        let visual = VisualPipeline::from_manifest(&manifest);
        let json = visual.to_json().unwrap();
        let restored = VisualPipeline::from_json(&json).unwrap();

        assert_eq!(restored.nodes.len(), 3);
        assert_eq!(restored.edges.len(), 2);
    }

    #[test]
    fn test_visual_to_manifest() {
        let manifest = test_manifest();
        let visual = VisualPipeline::from_manifest(&manifest);
        let restored_manifest = visual.to_manifest().unwrap();

        assert_eq!(restored_manifest.source.len(), 1);
        assert_eq!(restored_manifest.transform.len(), 1);
        assert_eq!(restored_manifest.sink.len(), 1);
        assert_eq!(restored_manifest.source[0].name, "roads");
    }
}
