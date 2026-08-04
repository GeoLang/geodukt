//! Simplify transform — reduce vertex count using Douglas-Peucker algorithm.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{
    FeatureGeometry, LineString, MultiLineString, MultiPolygon, Polygon, Ring,
};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use topoi_core::simplify;

/// Simplify operation: reduces geometry complexity.
pub struct SimplifyTransform;

impl TransformOp for SimplifyTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let epsilon = crate::params::float(params, "simplify", "epsilon")?;

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

fn simplify_geometry(geom: &FeatureGeometry, epsilon: f64) -> FeatureGeometry {
    match geom {
        FeatureGeometry::LineString(ls) => FeatureGeometry::LineString(simplify_line(ls, epsilon)),
        FeatureGeometry::Polygon(p) => FeatureGeometry::Polygon(simplify_polygon(p, epsilon)),
        FeatureGeometry::MultiLineString(mls) => {
            FeatureGeometry::MultiLineString(MultiLineString::new(
                mls.linestrings()
                    .iter()
                    .map(|ls| simplify_line(ls, epsilon))
                    .collect(),
            ))
        }
        FeatureGeometry::MultiPolygon(mp) => FeatureGeometry::MultiPolygon(MultiPolygon::new(
            mp.polygons()
                .iter()
                .map(|p| simplify_polygon(p, epsilon))
                .collect(),
        )),
        other => other.clone(),
    }
}

fn simplify_line(line: &LineString, epsilon: f64) -> LineString {
    LineString::new(simplify(line.coords(), epsilon))
}

fn simplify_ring(ring: &Ring, epsilon: f64) -> Ring {
    Ring::new(simplify(ring.coords(), epsilon))
}

fn simplify_polygon(poly: &Polygon, epsilon: f64) -> Polygon {
    Polygon::new(
        simplify_ring(poly.exterior(), epsilon),
        poly.interiors()
            .iter()
            .map(|r| simplify_ring(r, epsilon))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::geometry::Coord;

    #[test]
    fn test_simplify_linestring() {
        // Create a line with many points that can be simplified
        let line = LineString::new(vec![
            Coord::new(0.0, 0.0),
            Coord::new(0.5, 0.001), // Nearly collinear
            Coord::new(1.0, 0.0),
            Coord::new(1.5, 0.001), // Nearly collinear
            Coord::new(2.0, 0.0),
        ]);
        let features = vec![Feature {
            geometry: FeatureGeometry::LineString(line),
            properties: HashMap::new(),
        }];
        let fc = FeatureCollection::new(features, None);
        let params = HashMap::from([("epsilon".into(), toml::Value::Float(0.01))]);

        let result = SimplifyTransform.apply(&fc, &params).unwrap();
        assert_eq!(result.len(), 1);
        // Simplified line should have fewer vertices
        if let FeatureGeometry::LineString(ls) = &result.features[0].geometry {
            assert!(ls.coords().len() < 5);
        }
    }
}
