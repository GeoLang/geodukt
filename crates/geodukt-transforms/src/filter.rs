//! Filter transform — filters features by property predicates.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::pipeline::{PipelineError, TransformOp};

/// Filter operation: keeps only features matching a property condition.
pub struct FilterTransform;

impl TransformOp for FilterTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let field = crate::params::string(params, "filter", "field")?;
        let equals = crate::params::require(params, "filter", "equals")?;

        let features: Vec<Feature> = input
            .features
            .iter()
            .filter(|f| match (f.properties.get(field), equals) {
                (Some(Value::String(s)), toml::Value::String(expected)) => s == expected,
                (Some(Value::Integer(n)), toml::Value::Integer(expected)) => n == expected,
                (Some(Value::Float(n)), toml::Value::Float(expected)) => {
                    (n - expected).abs() < f64::EPSILON
                }
                (Some(Value::Bool(b)), toml::Value::Boolean(expected)) => b == expected,
                _ => false,
            })
            .cloned()
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::geometry::{FeatureGeometry, Point};

    #[test]
    fn test_filter_by_string() {
        let features = vec![
            Feature {
                geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
                properties: HashMap::from([("type".into(), Value::String("road".into()))]),
            },
            Feature {
                geometry: FeatureGeometry::Point(Point::new(1.0, 1.0)),
                properties: HashMap::from([("type".into(), Value::String("building".into()))]),
            },
        ];
        let fc = FeatureCollection::new(features, None);
        let params = HashMap::from([
            ("field".into(), toml::Value::String("type".into())),
            ("equals".into(), toml::Value::String("road".into())),
        ]);

        let result = FilterTransform.apply(&fc, &params).unwrap();
        assert_eq!(result.len(), 1);
    }
}
