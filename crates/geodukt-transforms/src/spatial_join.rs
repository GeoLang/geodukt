//! Spatial join transform — joins features based on spatial relationships.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{Coord, FeatureGeometry, LineString, Polygon, envelope};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use topoi_core::{contains, intersection, segment_intersection};

/// Spatial join operation: enriches features with properties from spatially related features.
/// Requires a secondary dataset loaded via `join_to` param.
#[derive(Default)]
pub struct SpatialJoinTransform {
    /// The dataset to join against.
    pub join_dataset: Option<FeatureCollection>,
}

impl SpatialJoinTransform {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_dataset(dataset: FeatureCollection) -> Self {
        Self {
            join_dataset: Some(dataset),
        }
    }
}

impl TransformOp for SpatialJoinTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let join_type = params
            .get("join_type")
            .and_then(|v: &toml::Value| v.as_str())
            .unwrap_or("intersects");

        let join_data = self
            .join_dataset
            .as_ref()
            .ok_or_else(|| PipelineError::Transform {
                name: "spatial_join".into(),
                message: "no join dataset provided".into(),
            })?;

        let features: Vec<Feature> = input
            .features
            .iter()
            .map(|f| {
                let mut props = f.properties.clone();
                // Find matching features from join dataset
                for jf in &join_data.features {
                    let matches = match join_type {
                        "contains" => geometry_contains(&f.geometry, &jf.geometry),
                        "within" => geometry_contains(&jf.geometry, &f.geometry),
                        _ => geometry_intersects(&f.geometry, &jf.geometry),
                    };
                    if matches {
                        // Merge properties with prefix
                        for (k, v) in &jf.properties {
                            props.insert(format!("joined_{k}"), v.clone());
                        }
                        break; // Take first match
                    }
                }
                Feature {
                    geometry: f.geometry.clone(),
                    properties: props,
                }
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

fn geometry_contains(container: &FeatureGeometry, contained: &FeatureGeometry) -> bool {
    match (container, contained) {
        (FeatureGeometry::Polygon(p), FeatureGeometry::Point(pt)) => contains(p, &pt.0),
        (FeatureGeometry::MultiPolygon(mp), FeatureGeometry::Point(pt)) => {
            mp.polygons().iter().any(|p| contains(p, &pt.0))
        }
        _ => false,
    }
}

/// The puntal, linear and areal members a geometry decomposes into, with
/// collections flattened.
#[derive(Default)]
struct Parts<'a> {
    points: Vec<Coord>,
    lines: Vec<&'a LineString>,
    polygons: Vec<&'a Polygon>,
}

fn parts(geom: &FeatureGeometry) -> Parts<'_> {
    let mut out = Parts::default();
    collect(geom, &mut out);
    out
}

fn collect<'a>(geom: &'a FeatureGeometry, out: &mut Parts<'a>) {
    match geom {
        FeatureGeometry::Point(p) => out.points.push(p.0),
        FeatureGeometry::MultiPoint(mp) => out.points.extend(mp.points().iter().map(|p| p.0)),
        FeatureGeometry::LineString(ls) => out.lines.push(ls),
        FeatureGeometry::MultiLineString(mls) => out.lines.extend(mls.linestrings()),
        FeatureGeometry::Polygon(p) => out.polygons.push(p),
        FeatureGeometry::MultiPolygon(mp) => out.polygons.extend(mp.polygons()),
        FeatureGeometry::GeometryCollection(members) => {
            for member in members {
                collect(member, out);
            }
        }
    }
}

/// True when the two geometries share at least one point.
fn geometry_intersects(a: &FeatureGeometry, b: &FeatureGeometry) -> bool {
    match (envelope(a), envelope(b)) {
        (Some(ea), Some(eb)) if ea.intersects(&eb) => {}
        _ => return false,
    }

    let (a, b) = (parts(a), parts(b));

    a.points.iter().any(|p| point_touches(*p, &b))
        || b.points.iter().any(|p| point_touches(*p, &a))
        || a.lines
            .iter()
            .any(|la| b.lines.iter().any(|lb| lines_cross(la, lb)))
        || a.lines
            .iter()
            .any(|l| b.polygons.iter().any(|p| line_meets_polygon(l, p)))
        || b.lines
            .iter()
            .any(|l| a.polygons.iter().any(|p| line_meets_polygon(l, p)))
        || a.polygons
            .iter()
            .any(|pa| b.polygons.iter().any(|pb| polygons_meet(pa, pb)))
}

fn point_touches(p: Coord, other: &Parts<'_>) -> bool {
    other.points.contains(&p)
        || other.lines.iter().any(|l| point_on_line(p, l))
        || other
            .polygons
            .iter()
            .any(|poly| contains(poly, &p) || rings(poly).any(|ring| point_on_coords(p, ring)))
}

fn rings(poly: &Polygon) -> impl Iterator<Item = &[Coord]> {
    std::iter::once(poly.exterior().coords()).chain(poly.interiors().iter().map(|r| r.coords()))
}

fn point_on_line(p: Coord, line: &LineString) -> bool {
    point_on_coords(p, line.coords())
}

fn point_on_coords(p: Coord, coords: &[Coord]) -> bool {
    coords.windows(2).any(|w| point_on_segment(p, w[0], w[1]))
}

/// Within a whisker of the segment, scaled so the test survives coordinates far
/// from the origin.
fn point_on_segment(p: Coord, a: Coord, b: Coord) -> bool {
    let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
    let scale = (b.x - a.x).abs() + (b.y - a.y).abs() + 1.0;
    if cross.abs() > 1e-12 * scale {
        return false;
    }
    let dot = (p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y);
    let len_sq = (b.x - a.x).powi(2) + (b.y - a.y).powi(2);
    if len_sq == 0.0 {
        return p == a;
    }
    (0.0..=len_sq).contains(&dot)
}

fn segments_meet(a: &[Coord], b: &[Coord]) -> bool {
    a.windows(2).any(|p| {
        b.windows(2).any(|q| {
            segment_intersection(p[0], p[1], q[0], q[1]).is_some()
                // collinear overlap gives no crossing point, so test the endpoints
                || point_on_segment(p[0], q[0], q[1])
                || point_on_segment(p[1], q[0], q[1])
                || point_on_segment(q[0], p[0], p[1])
                || point_on_segment(q[1], p[0], p[1])
        })
    })
}

fn lines_cross(a: &LineString, b: &LineString) -> bool {
    segments_meet(a.coords(), b.coords())
}

fn line_meets_polygon(line: &LineString, poly: &Polygon) -> bool {
    line.coords().iter().any(|c| contains(poly, c))
        || rings(poly).any(|ring| segments_meet(line.coords(), ring))
}

fn polygons_meet(a: &Polygon, b: &Polygon) -> bool {
    // an overlap of any area, plus the touching cases that leave no area behind
    !intersection(a, b).polygons().is_empty()
        || rings(a).any(|ra| rings(b).any(|rb| segments_meet(ra, rb)))
        || a.exterior()
            .coords()
            .first()
            .is_some_and(|c| contains(b, c))
        || b.exterior()
            .coords()
            .first()
            .is_some_and(|c| contains(a, c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::feature::Value;
    use geodukt_core::geometry::{LineString, Point, Ring};

    fn square(min: f64, max: f64) -> Polygon {
        Polygon::new(
            Ring::new(vec![
                Coord::new(min, min),
                Coord::new(max, min),
                Coord::new(max, max),
                Coord::new(min, max),
                Coord::new(min, min),
            ]),
            vec![],
        )
    }

    #[test]
    fn test_spatial_join_intersects() {
        let join_fc = FeatureCollection::new(
            vec![Feature {
                geometry: FeatureGeometry::Polygon(square(0.0, 10.0)),
                properties: HashMap::from([("zone".into(), Value::String("residential".into()))]),
            }],
            None,
        );

        let input_features = vec![
            Feature {
                geometry: FeatureGeometry::Point(Point::new(5.0, 5.0)),
                properties: HashMap::from([("id".into(), Value::Integer(1))]),
            },
            Feature {
                geometry: FeatureGeometry::Point(Point::new(20.0, 20.0)),
                properties: HashMap::from([("id".into(), Value::Integer(2))]),
            },
        ];
        let input_fc = FeatureCollection::new(input_features, None);

        let transform = SpatialJoinTransform::with_dataset(join_fc);
        let result = transform.apply(&input_fc, &HashMap::new()).unwrap();
        assert_eq!(result.len(), 2);
        // First point is inside polygon, should have joined property
        assert_eq!(
            result.features[0].properties.get("joined_zone"),
            Some(&Value::String("residential".into()))
        );
        // Second point is outside, no joined property
        assert!(!result.features[1].properties.contains_key("joined_zone"));
    }

    #[test]
    fn test_intersects_covers_lines_and_polygons() {
        let poly = FeatureGeometry::Polygon(square(0.0, 10.0));
        let crossing = FeatureGeometry::LineString(LineString::new(vec![
            Coord::new(-5.0, 5.0),
            Coord::new(15.0, 5.0),
        ]));
        let clear = FeatureGeometry::LineString(LineString::new(vec![
            Coord::new(-5.0, 50.0),
            Coord::new(15.0, 50.0),
        ]));
        assert!(geometry_intersects(&poly, &crossing));
        assert!(!geometry_intersects(&poly, &clear));

        // a polygon fully inside another still intersects it
        let inner = FeatureGeometry::Polygon(square(2.0, 4.0));
        assert!(geometry_intersects(&poly, &inner));

        // sharing only an edge counts, the same as geo's predicate did
        let neighbour = FeatureGeometry::Polygon(square(10.0, 20.0));
        assert!(geometry_intersects(&poly, &neighbour));

        let apart = FeatureGeometry::Polygon(square(30.0, 40.0));
        assert!(!geometry_intersects(&poly, &apart));
    }

    #[test]
    fn test_contains_only_answers_for_polygon_and_point() {
        let poly = FeatureGeometry::Polygon(square(0.0, 10.0));
        let inside = FeatureGeometry::Point(Point::new(1.0, 1.0));
        let outside = FeatureGeometry::Point(Point::new(11.0, 1.0));
        assert!(geometry_contains(&poly, &inside));
        assert!(!geometry_contains(&poly, &outside));
        assert!(!geometry_contains(&inside, &poly));
    }
}
