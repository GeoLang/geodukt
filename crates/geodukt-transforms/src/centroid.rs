//! Centroid transform — replaces geometries with their centroid point.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{FeatureGeometry, Point};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use topoi_core::centroid;

/// Centroid operation: replaces each geometry with its centroid.
pub struct CentroidTransform;

impl TransformOp for CentroidTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        _params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let features: Vec<Feature> = input
            .features
            .iter()
            .filter_map(|f| {
                let c = centroid(&f.geometry)?;
                Some(Feature {
                    geometry: FeatureGeometry::Point(Point::new(c.x, c.y)),
                    properties: f.properties.clone(),
                })
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::feature::Value;
    use geodukt_core::geometry::{Coord, Polygon, Ring};

    #[test]
    fn test_centroid_polygon() {
        let poly = Polygon::new(
            Ring::new(vec![
                Coord::new(0.0, 0.0),
                Coord::new(4.0, 0.0),
                Coord::new(4.0, 4.0),
                Coord::new(0.0, 4.0),
                Coord::new(0.0, 0.0),
            ]),
            vec![],
        );
        let features = vec![Feature {
            geometry: FeatureGeometry::Polygon(poly),
            properties: HashMap::from([("name".into(), Value::String("square".into()))]),
        }];
        let fc = FeatureCollection::new(features, None);

        let result = CentroidTransform.apply(&fc, &HashMap::new()).unwrap();
        assert_eq!(result.len(), 1);
        if let FeatureGeometry::Point(p) = &result.features[0].geometry {
            assert!((p.0.x - 2.0).abs() < f64::EPSILON);
            assert!((p.0.y - 2.0).abs() < f64::EPSILON);
        } else {
            panic!("expected Point geometry");
        }
    }
}
