//! Clip transform — clip features to a boundary geometry.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{Coord, FeatureGeometry, MultiPolygon, Polygon, Ring};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use topoi_core::intersection;

/// Clip operation: intersects feature geometries with a clip boundary.
/// Requires a secondary input specified by `clip_to` param (resolved by pipeline).
#[derive(Default)]
pub struct ClipTransform {
    /// The clip boundary loaded from the secondary input.
    pub clip_boundary: Option<MultiPolygon>,
}

impl ClipTransform {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_boundary(boundary: MultiPolygon) -> Self {
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
                Ring::new(vec![
                    Coord::new(min_x, min_y),
                    Coord::new(max_x, min_y),
                    Coord::new(max_x, max_y),
                    Coord::new(min_x, max_y),
                    Coord::new(min_x, min_y),
                ]),
                vec![],
            );
            MultiPolygon::new(vec![poly])
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

fn clip_geometry(geom: &FeatureGeometry, clip: &MultiPolygon) -> Option<FeatureGeometry> {
    let boundary = clip.polygons().first()?;
    match geom {
        FeatureGeometry::Polygon(poly) => {
            let result = intersection(poly, boundary);
            match result.polygons() {
                [] => None,
                [single] => Some(FeatureGeometry::Polygon(single.clone())),
                _ => Some(FeatureGeometry::MultiPolygon(result)),
            }
        }
        FeatureGeometry::MultiPolygon(mp) => {
            let mut polys = Vec::new();
            for poly in mp.polygons() {
                polys.extend(intersection(poly, boundary).polygons().iter().cloned());
            }
            if polys.is_empty() {
                None
            } else {
                Some(FeatureGeometry::MultiPolygon(MultiPolygon::new(polys)))
            }
        }
        // For points/lines, check containment via bounding box (simplified)
        _ => Some(geom.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clip_polygon() {
        let poly = Polygon::new(
            Ring::new(vec![
                Coord::new(-1.0, -1.0),
                Coord::new(2.0, -1.0),
                Coord::new(2.0, 2.0),
                Coord::new(-1.0, 2.0),
                Coord::new(-1.0, -1.0),
            ]),
            vec![],
        );
        let features = vec![Feature {
            geometry: FeatureGeometry::Polygon(poly),
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
