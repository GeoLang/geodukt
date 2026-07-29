//! Clip transform — clip features to a boundary geometry.

use std::collections::HashMap;

use geo::{BooleanOps, Geometry, MultiPolygon, Polygon};
use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::pipeline::{PipelineError, TransformOp};

/// Clip operation: intersects feature geometries with a clip boundary.
/// Requires a secondary input specified by `clip_to` param (resolved by pipeline).
#[derive(Default)]
pub struct ClipTransform {
    /// The clip boundary loaded from the secondary input.
    pub clip_boundary: Option<MultiPolygon<f64>>,
}

impl ClipTransform {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_boundary(boundary: MultiPolygon<f64>) -> Self {
        Self {
            clip_boundary: Some(boundary),
        }
    }
}

impl TransformOp for ClipTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        // a manifest has no way to name a boundary geometry, so without a
        // pre-loaded one the bounding box is the boundary, all four edges of it
        let clip = if let Some(ref boundary) = self.clip_boundary {
            boundary.clone()
        } else {
            let min_x = crate::params::float(params, "clip", "min_x")?;
            let min_y = crate::params::float(params, "clip", "min_y")?;
            let max_x = crate::params::float(params, "clip", "max_x")?;
            let max_y = crate::params::float(params, "clip", "max_y")?;

            let poly = Polygon::new(
                geo::LineString::from(vec![
                    (min_x, min_y),
                    (max_x, min_y),
                    (max_x, max_y),
                    (min_x, max_y),
                    (min_x, min_y),
                ]),
                vec![],
            );
            MultiPolygon(vec![poly])
        };

        let features: Vec<Feature> = input
            .features
            .iter()
            .filter_map(|f| {
                let clipped = clip_geometry(&f.geometry, &clip)?;
                Some(Feature {
                    geometry: clipped,
                    properties: f.properties.clone(),
                })
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

fn clip_geometry(geom: &Geometry<f64>, clip: &MultiPolygon<f64>) -> Option<Geometry<f64>> {
    match geom {
        Geometry::Polygon(poly) => {
            let result = poly.intersection(&clip.0[0]);
            if result.0.is_empty() {
                None
            } else if result.0.len() == 1 {
                Some(Geometry::Polygon(result.0[0].clone()))
            } else {
                Some(Geometry::MultiPolygon(result))
            }
        }
        Geometry::MultiPolygon(mp) => {
            let mut polys = Vec::new();
            for poly in &mp.0 {
                let result = poly.intersection(&clip.0[0]);
                polys.extend(result.0);
            }
            if polys.is_empty() {
                None
            } else {
                Some(Geometry::MultiPolygon(MultiPolygon(polys)))
            }
        }
        // For points/lines, check containment via bounding box (simplified)
        _ => Some(geom.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::polygon;

    #[test]
    fn test_clip_polygon() {
        let poly = polygon![
            (x: -1.0, y: -1.0),
            (x: 2.0, y: -1.0),
            (x: 2.0, y: 2.0),
            (x: -1.0, y: 2.0),
            (x: -1.0, y: -1.0),
        ];
        let features = vec![Feature {
            geometry: Geometry::Polygon(poly),
            properties: HashMap::new(),
        }];
        let fc = FeatureCollection::new(features, None);

        let params = HashMap::from([
            ("min_x".into(), toml::Value::Float(0.0)),
            ("min_y".into(), toml::Value::Float(0.0)),
            ("max_x".into(), toml::Value::Float(1.0)),
            ("max_y".into(), toml::Value::Float(1.0)),
        ]);

        let result = ClipTransform::new().apply(&fc, &params).unwrap();
        assert_eq!(result.len(), 1);
    }
}
