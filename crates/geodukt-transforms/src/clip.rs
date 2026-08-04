//! Clip transform — clip features to a boundary geometry.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{Coord, MultiPolygon, Polygon, Ring};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use topoi_core::clip_to_boundary;

/// Clip operation: cuts feature geometries down to a clip boundary. Polygons
/// are intersected, lines are cut at the crossings, points outside are dropped,
/// and a feature left with nothing goes with them.
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
                Some(Feature {
                    geometry: clip_to_boundary(&f.geometry, &clip)?,
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
    use geodukt_core::geometry::{FeatureGeometry, LineString, Point};

    /// the unit square
    fn params() -> HashMap<String, toml::Value> {
        HashMap::from([
            ("min_x".into(), toml::Value::Float(0.0)),
            ("min_y".into(), toml::Value::Float(0.0)),
            ("max_x".into(), toml::Value::Float(1.0)),
            ("max_y".into(), toml::Value::Float(1.0)),
        ])
    }

    fn feature(geometry: FeatureGeometry) -> Feature {
        Feature {
            geometry,
            properties: HashMap::new(),
        }
    }

    fn clipped(features: Vec<Feature>) -> FeatureCollection {
        ClipTransform::new()
            .apply(&FeatureCollection::new(features, None), &params())
            .unwrap()
    }

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
        let result = clipped(vec![feature(FeatureGeometry::Polygon(poly))]);
        assert_eq!(result.len(), 1);
    }

    /// a line leaving the box is cut where it crosses, not carried through
    #[test]
    fn test_clip_cuts_a_line_at_the_boundary() {
        // the crossings land on quarter and half of the span, both exact, so
        // the cut coordinates are exact too
        let line = LineString::new(vec![Coord::new(-1.0, 0.5), Coord::new(3.0, 0.5)]);
        let result = clipped(vec![feature(FeatureGeometry::LineString(line))]);
        assert_eq!(result.len(), 1);

        let FeatureGeometry::LineString(cut) = &result.features[0].geometry else {
            panic!(
                "a line in, a line out, got {:?}",
                result.features[0].geometry
            );
        };
        assert_eq!(
            cut.coords(),
            [Coord::new(0.0, 0.5), Coord::new(1.0, 0.5)].as_slice()
        );
    }

    #[test]
    fn test_clip_drops_a_point_outside_the_boundary() {
        let result = clipped(vec![
            feature(FeatureGeometry::Point(Point::new(0.5, 0.5))),
            feature(FeatureGeometry::Point(Point::new(5.0, 5.0))),
        ]);
        assert_eq!(result.len(), 1, "only the point inside the box survives");
        let FeatureGeometry::Point(p) = &result.features[0].geometry else {
            panic!("a point in, a point out");
        };
        assert_eq!(*p, Point::new(0.5, 0.5));
    }
}
