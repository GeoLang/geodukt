//! Feature representation — the unit of data flowing through the pipeline.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::geometry::FeatureGeometry;

/// A single geospatial feature with geometry and properties.
#[derive(Debug, Clone)]
pub struct Feature {
    pub geometry: FeatureGeometry,
    pub properties: Properties,
}

/// Key-value property map for a feature.
pub type Properties = HashMap<String, Value>;

/// Property value types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

/// A collection of features (the data unit passed between pipeline stages).
#[derive(Debug, Clone)]
pub struct FeatureCollection {
    pub features: Vec<Feature>,
    pub crs: Option<String>,
}

impl FeatureCollection {
    pub fn new(features: Vec<Feature>, crs: Option<String>) -> Self {
        Self { features, crs }
    }

    pub fn empty() -> Self {
        Self {
            features: Vec::new(),
            crs: None,
        }
    }

    pub fn len(&self) -> usize {
        self.features.len()
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;

    #[test]
    fn test_feature_collection() {
        let f = Feature {
            geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
            properties: HashMap::from([("name".into(), Value::String("test".into()))]),
        };
        let fc = FeatureCollection::new(vec![f], Some("EPSG:4326".into()));
        assert_eq!(fc.len(), 1);
        assert!(!fc.is_empty());
        assert_eq!(fc.crs, Some("EPSG:4326".into()));
    }
}
