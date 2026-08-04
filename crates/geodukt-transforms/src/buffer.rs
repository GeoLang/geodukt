//! Buffer transform: expands or shrinks geometries by a metric distance.
//!
//! Offset math is delegated to topoi's `buffer_geometry` (i_overlay based):
//! points become circles, lines become round-capped capsules, and polygons grow
//! outward with round joins (or shrink inward for a negative distance). To make
//! `distance` mean meters regardless of the input CRS, each geometry is
//! projected into a local azimuthal-equidistant plane centered on the
//! collection, buffered there, then projected back. The aeqd plane keeps
//! distances from its center true, so the buffer width is metric even for
//! lon/lat input.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{Coord, FeatureGeometry, envelope, map_coords};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use projicio_core::Transform;
use topoi_core::buffer_geometry;

const DEFAULT_SEGMENTS: usize = 64;

/// Buffer operation: offsets point, line and polygon geometries by a distance in meters.
pub struct BufferTransform;

impl TransformOp for BufferTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let distance = crate::params::float(params, "buffer", "distance")?;

        let segments = params
            .get("segments")
            .and_then(|v: &toml::Value| v.as_integer())
            .map(|s| (s.max(3)) as usize)
            .unwrap_or(DEFAULT_SEGMENTS);

        buffer_collection(input, distance, segments)
    }
}

/// Buffer every feature and return a new collection with the same CRS.
fn buffer_collection(
    input: &FeatureCollection,
    distance: f64,
    segments: usize,
) -> Result<FeatureCollection, PipelineError> {
    let crs = input.crs.as_deref().unwrap_or("EPSG:4326");
    let (lon, lat) = collection_center_lonlat(input, crs)?;
    let aeqd = local_aeqd(lon, lat);
    let to_local = make_proj(crs, &aeqd)?;
    let to_crs = make_proj(&aeqd, crs)?;

    let features = input
        .features
        .iter()
        .map(|f| {
            let local = map_coords(&f.geometry, &|c| project(&to_local, c));
            let buffered = buffer_geometry(&local, distance, segments);
            let geometry = map_coords(&FeatureGeometry::MultiPolygon(buffered), &|c| {
                project(&to_crs, c)
            });
            Feature {
                geometry,
                properties: f.properties.clone(),
            }
        })
        .collect();

    Ok(FeatureCollection::new(features, input.crs.clone()))
}

fn project(transform: &Transform, c: Coord) -> Coord {
    let (x, y) = transform.convert(c.x, c.y).unwrap_or((c.x, c.y));
    Coord::new(x, y)
}

/// Center of the collection expressed in lon/lat, used to anchor the local plane.
fn collection_center_lonlat(
    input: &FeatureCollection,
    crs: &str,
) -> Result<(f64, f64), PipelineError> {
    let bounds = input
        .features
        .iter()
        .filter_map(|f| envelope(&f.geometry))
        .reduce(|acc, env| acc.union(&env));

    let center = match bounds {
        Some(env) => (env.center_x(), env.center_y()),
        None => (0.0, 0.0),
    };

    if crs.eq_ignore_ascii_case("EPSG:4326") {
        return Ok(center);
    }

    let to_geo = make_proj(crs, "EPSG:4326")?;
    to_geo
        .convert(center.0, center.1)
        .map_err(|e| transform_err(format!("failed to locate centroid in lon/lat: {e}")))
}

fn local_aeqd(lon: f64, lat: f64) -> String {
    format!("+proj=aeqd +lat_0={lat} +lon_0={lon} +datum=WGS84 +units=m +no_defs +type=crs")
}

fn make_proj(from: &str, to: &str) -> Result<Transform, PipelineError> {
    Transform::new(from, to).map_err(|e| transform_err(format!("failed to create projection: {e}")))
}

fn transform_err(message: String) -> PipelineError {
    PipelineError::Transform {
        name: "buffer".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::feature::{Feature, Value};
    use geodukt_core::geometry::{LineString, MultiPolygon, Point, Polygon, Ring};
    use std::f64::consts::PI;

    fn fc(geometry: FeatureGeometry) -> FeatureCollection {
        FeatureCollection::new(
            vec![Feature {
                geometry,
                properties: HashMap::from([("id".into(), Value::Integer(1))]),
            }],
            Some("EPSG:4326".into()),
        )
    }

    fn params(distance: f64) -> HashMap<String, toml::Value> {
        HashMap::from([("distance".into(), toml::Value::Float(distance))])
    }

    fn multipolygon(geom: &FeatureGeometry) -> &MultiPolygon {
        match geom {
            FeatureGeometry::MultiPolygon(mp) => mp,
            other => panic!("expected MultiPolygon, got {other:?}"),
        }
    }

    /// Metric measurements on the lon/lat output, on a sphere of the earth's
    /// authalic radius. Good to a fraction of a percent at these sizes, which is
    /// well inside the tolerances the buffer shape is checked against.
    mod geodesic {
        use super::*;

        const RADIUS: f64 = 6_371_008.8;

        pub fn distance(a: Coord, b: Coord) -> f64 {
            let (lat1, lat2) = (a.y.to_radians(), b.y.to_radians());
            let dlat = lat2 - lat1;
            let dlon = (b.x - a.x).to_radians();
            let h =
                (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
            2.0 * RADIUS * h.sqrt().asin()
        }

        pub fn length(coords: &[Coord]) -> f64 {
            coords.windows(2).map(|w| distance(w[0], w[1])).sum()
        }

        fn closed(coords: &[Coord]) -> Vec<Coord> {
            let mut out = coords.to_vec();
            match (out.first().copied(), out.last().copied()) {
                (Some(first), Some(last)) if first != last => out.push(first),
                _ => {}
            }
            out
        }

        /// Perimeter of the closed exterior ring.
        pub fn perimeter(poly: &Polygon) -> f64 {
            length(&closed(poly.exterior().coords()))
        }

        /// Spherical excess area of a ring, unsigned.
        fn ring_area(coords: &[Coord]) -> f64 {
            let ring = closed(coords);
            let sum: f64 = ring
                .windows(2)
                .map(|w| {
                    (w[1].x - w[0].x).to_radians()
                        * (2.0 + w[0].y.to_radians().sin() + w[1].y.to_radians().sin())
                })
                .sum();
            (RADIUS * RADIUS * sum / 2.0).abs()
        }

        pub fn polygon_area(poly: &Polygon) -> f64 {
            let holes: f64 = poly.interiors().iter().map(|r| ring_area(r.coords())).sum();
            ring_area(poly.exterior().coords()) - holes
        }

        pub fn area(mp: &MultiPolygon) -> f64 {
            mp.polygons().iter().map(polygon_area).sum()
        }
    }

    fn polygon(coords: &[(f64, f64)]) -> Polygon {
        let mut ring: Vec<Coord> = coords.iter().map(|(x, y)| Coord::new(*x, *y)).collect();
        ring.push(ring[0]);
        Polygon::new(Ring::new(ring), vec![])
    }

    /// The helper has to measure a shape whose area is known analytically, or a
    /// factor lost in it would show up as a buffer that looks wrong.
    #[test]
    fn test_geodesic_area_helper_matches_a_spherical_rectangle() {
        let rect = polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]);
        let radius: f64 = 6_371_008.8;
        let expected = radius * radius * (1.0f64).to_radians() * (1.0f64).to_radians().sin();
        let measured = geodesic::polygon_area(&rect);
        assert!(
            (measured - expected).abs() / expected < 1e-9,
            "measured {measured}, expected {expected}"
        );
    }

    #[test]
    fn test_buffer_point_area() {
        let r = 100.0;
        let result = BufferTransform
            .apply(
                &fc(FeatureGeometry::Point(Point::new(8.55, 47.37))),
                &params(r),
            )
            .unwrap();

        let area = geodesic::area(multipolygon(&result.features[0].geometry));
        let expected = PI * r * r;
        assert!(
            (area - expected).abs() / expected < 0.02,
            "point buffer area {area} not within 2% of {expected}"
        );
    }

    #[test]
    fn test_buffer_line_area() {
        let r = 100.0;
        let line = LineString::new(vec![Coord::new(8.50, 47.37), Coord::new(8.52, 47.37)]);
        let length = geodesic::length(line.coords());
        let result = BufferTransform
            .apply(&fc(FeatureGeometry::LineString(line)), &params(r))
            .unwrap();

        let area = geodesic::area(multipolygon(&result.features[0].geometry));
        let expected = 2.0 * r * length + PI * r * r;
        assert!(
            (area - expected).abs() / expected < 0.05,
            "line buffer area {area} not within 5% of {expected} (length {length})"
        );
    }

    #[test]
    fn test_buffer_polygon_grows_and_contains() {
        let r = 100.0;
        let original = polygon(&[(8.50, 47.36), (8.52, 47.36), (8.52, 47.38), (8.50, 47.38)]);
        let orig_area = geodesic::polygon_area(&original);
        let perimeter = geodesic::perimeter(&original);

        let result = BufferTransform
            .apply(&fc(FeatureGeometry::Polygon(original.clone())), &params(r))
            .unwrap();
        let buffered = multipolygon(&result.features[0].geometry);

        // nothing of the original sticks out of the buffer
        let outside = topoi_core::difference(&original, buffered);
        assert!(
            outside.area() < 1e-12,
            "buffered polygon must contain the original, {} left outside",
            outside.area()
        );

        let area = geodesic::area(buffered);
        let expected = orig_area + perimeter * r + PI * r * r;
        assert!(
            (area - expected).abs() / expected < 0.05,
            "polygon buffer area {area} not within 5% of {expected}"
        );
    }

    #[test]
    fn test_buffer_negative_shrinks_polygon() {
        let original = polygon(&[(8.50, 47.36), (8.55, 47.36), (8.55, 47.40), (8.50, 47.40)]);
        let orig_area = geodesic::polygon_area(&original);

        let result = BufferTransform
            .apply(&fc(FeatureGeometry::Polygon(original)), &params(-100.0))
            .unwrap();
        let area = geodesic::area(multipolygon(&result.features[0].geometry));

        assert!(
            area > 0.0 && area < orig_area,
            "negative buffer should shrink area: got {area}, original {orig_area}"
        );
    }

    /// A buffer with no distance used to fall back to 1 metre, so a caller who
    /// meant 500 got a metre and was told nothing.
    #[test]
    fn test_buffer_without_a_distance_fails_loud() {
        let err = BufferTransform
            .apply(
                &fc(FeatureGeometry::Point(Point::new(0.0, 0.0))),
                &HashMap::new(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("distance"), "{err}");
    }

    #[test]
    fn test_buffer_roundtrips_through_registry() {
        use crate::registry::default_registry;

        let registry = default_registry();
        let op = registry.get("buffer").expect("buffer registered");
        let input = fc(FeatureGeometry::Point(Point::new(0.0, 0.0)));

        let result = op.apply(&input, &params(5.0)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.crs, Some("EPSG:4326".into()));
        let mp = multipolygon(&result.features[0].geometry);
        assert!(mp.area() > 0.0);
        // properties survive the transform
        assert_eq!(
            result.features[0].properties.get("id"),
            Some(&Value::Integer(1))
        );
    }

    #[test]
    fn test_buffer_point_is_multipolygon() {
        let result = BufferTransform
            .apply(
                &fc(FeatureGeometry::Point(Point::new(0.0, 0.0))),
                &params(5.0),
            )
            .unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(
            result.features[0].geometry,
            FeatureGeometry::MultiPolygon(_)
        ));
    }
}
