//! Simplify transform — reduce vertex count using Douglas-Peucker algorithm.

use std::collections::HashMap;

use geo::{Geometry, Simplify};
use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::pipeline::{PipelineError, TransformOp};

/// Simplify operation: reduces geometry complexity.
pub struct SimplifyTransform;

impl TransformOp for SimplifyTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let epsilon = params
            .get("epsilon")
            .and_then(|v: &toml::Value| v.as_float())
            .unwrap_or(0.001);

        let features: Vec<Feature> = input
            .features
            .iter()
            .map(|f| Feature {
                geometry: simplify_geometry(&f.geometry, epsilon),
                properties: f.properties.clone(),
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

fn simplify_geometry(geom: &Geometry<f64>, epsilon: f64) -> Geometry<f64> {
    match geom {
        Geometry::LineString(ls) => Geometry::LineString(ls.simplify(epsilon)),
        Geometry::Polygon(p) => Geometry::Polygon(p.simplify(epsilon)),
        Geometry::MultiLineString(mls) => Geometry::MultiLineString(mls.simplify(epsilon)),
        Geometry::MultiPolygon(mp) => Geometry::MultiPolygon(mp.simplify(epsilon)),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{Geometry, LineString};

    #[test]
    fn test_simplify_linestring() {
        // Create a line with many points that can be simplified
        let line = LineString::from(vec![
            (0.0, 0.0),
            (0.5, 0.001), // Nearly collinear
            (1.0, 0.0),
            (1.5, 0.001), // Nearly collinear
            (2.0, 0.0),
        ]);
        let features = vec![Feature {
            geometry: Geometry::LineString(line),
            properties: HashMap::new(),
        }];
        let fc = FeatureCollection::new(features, None);
        let params = HashMap::from([("epsilon".into(), toml::Value::Float(0.01))]);

        let result = SimplifyTransform.apply(&fc, &params).unwrap();
        assert_eq!(result.len(), 1);
        // Simplified line should have fewer vertices
        if let Geometry::LineString(ls) = &result.features[0].geometry {
            assert!(ls.0.len() < 5);
        }
    }
}
