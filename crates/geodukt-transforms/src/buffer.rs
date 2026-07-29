//! Buffer transform: expands or shrinks geometries by a metric distance.
//!
//! Offset math is delegated to geo's `Buffer` (i_overlay based): points become
//! circles, lines become round-capped capsules, and polygons grow outward with
//! round joins (or shrink inward for a negative distance). To make `distance`
//! mean meters regardless of the input CRS, each geometry is projected into a
//! local azimuthal-equidistant plane centered on the collection, buffered there,
//! then projected back. The aeqd plane keeps distances from its center true, so
//! the buffer width is metric even for lon/lat input.
//!
//! That projection step needs proj, so it only happens with the `reproject`
//! feature on (the default). Built without it, `distance` is CRS units instead,
//! which for lon/lat input means degrees.

use std::collections::HashMap;
use std::f64::consts::PI;

use geo::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
use geo::{Buffer, Geometry, MultiPolygon};
use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::pipeline::{PipelineError, TransformOp};

#[cfg(feature = "reproject")]
use geo::{BoundingRect, MapCoords};
#[cfg(feature = "reproject")]
use proj::Proj;

const DEFAULT_SEGMENTS: usize = 64;

/// Buffer operation: offsets point, line and polygon geometries by a distance in meters.
pub struct BufferTransform;

impl TransformOp for BufferTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let distance = params
            .get("distance")
            .and_then(|v: &toml::Value| v.as_float())
            .unwrap_or(1.0);

        let segments = params
            .get("segments")
            .and_then(|v: &toml::Value| v.as_integer())
            .map(|s| (s.max(3)) as usize)
            .unwrap_or(DEFAULT_SEGMENTS);

        buffer_collection(input, distance, segments)
    }
}

/// Buffer every feature and return a new collection with the same CRS.
#[cfg(feature = "reproject")]
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
            let local = f.geometry.map_coords(|c| project(&to_local, c));
            let buffered = buffer_local(&local, distance, segments);
            let geometry = Geometry::MultiPolygon(buffered).map_coords(|c| project(&to_crs, c));
            Feature {
                geometry,
                properties: f.properties.clone(),
            }
        })
        .collect();

    Ok(FeatureCollection::new(features, input.crs.clone()))
}

/// Fallback used when the `reproject` feature (and PROJ) is unavailable:
/// buffer directly in the input coordinate plane, distance in coordinate units.
#[cfg(not(feature = "reproject"))]
fn buffer_collection(
    input: &FeatureCollection,
    distance: f64,
    segments: usize,
) -> Result<FeatureCollection, PipelineError> {
    let features = input
        .features
        .iter()
        .map(|f| Feature {
            geometry: Geometry::MultiPolygon(buffer_local(&f.geometry, distance, segments)),
            properties: f.properties.clone(),
        })
        .collect();

    Ok(FeatureCollection::new(features, input.crs.clone()))
}

/// Planar buffer with round joins and caps approximated by `segments`-gon arcs.
/// Negative distance shrinks polygons; for points and lines it yields an empty result.
fn buffer_local(geom: &Geometry<f64>, distance: f64, segments: usize) -> MultiPolygon<f64> {
    let angle = 2.0 * PI / segments as f64;
    let style = BufferStyle::new(distance)
        .line_join(LineJoin::Round(angle))
        .line_cap(LineCap::Round(angle));
    geom.buffer_with_style(style)
}

#[cfg(feature = "reproject")]
fn project(proj: &Proj, c: geo::Coord<f64>) -> geo::Coord<f64> {
    let (x, y) = proj.convert((c.x, c.y)).unwrap_or((c.x, c.y));
    geo::Coord { x, y }
}

/// Center of the collection expressed in lon/lat, used to anchor the local plane.
#[cfg(feature = "reproject")]
fn collection_center_lonlat(
    input: &FeatureCollection,
    crs: &str,
) -> Result<(f64, f64), PipelineError> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for f in &input.features {
        if let Some(rect) = f.geometry.bounding_rect() {
            let (min, max) = (rect.min(), rect.max());
            bounds = Some(match bounds {
                None => (min.x, min.y, max.x, max.y),
                Some((minx, miny, maxx, maxy)) => (
                    minx.min(min.x),
                    miny.min(min.y),
                    maxx.max(max.x),
                    maxy.max(max.y),
                ),
            });
        }
    }

    let (minx, miny, maxx, maxy) = bounds.unwrap_or((0.0, 0.0, 0.0, 0.0));
    let center = ((minx + maxx) / 2.0, (miny + maxy) / 2.0);

    if crs.eq_ignore_ascii_case("EPSG:4326") {
        return Ok(center);
    }

    let to_geo = make_proj(crs, "EPSG:4326")?;
    to_geo
        .convert(center)
        .map_err(|e| transform_err(format!("failed to locate centroid in lon/lat: {e}")))
}

#[cfg(feature = "reproject")]
fn local_aeqd(lon: f64, lat: f64) -> String {
    format!("+proj=aeqd +lat_0={lat} +lon_0={lon} +datum=WGS84 +units=m +no_defs +type=crs")
}

#[cfg(feature = "reproject")]
fn make_proj(from: &str, to: &str) -> Result<Proj, PipelineError> {
    Proj::new_known_crs(from, to, None)
        .map_err(|e| transform_err(format!("failed to create projection: {e}")))
}

#[cfg(feature = "reproject")]
fn transform_err(message: String) -> PipelineError {
    PipelineError::Transform {
        name: "buffer".into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{Area, point};
    use geodukt_core::feature::Value;

    fn fc(geometry: Geometry<f64>) -> FeatureCollection {
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

    fn multipolygon(geom: &Geometry<f64>) -> &MultiPolygon<f64> {
        match geom {
            Geometry::MultiPolygon(mp) => mp,
            other => panic!("expected MultiPolygon, got {other:?}"),
        }
    }

    /// The metric contract only holds with the reproject feature. Without proj
    /// there is no local plane to buffer in, so `distance` is CRS units and a
    /// geodesic area check is meaningless.
    #[cfg(feature = "reproject")]
    mod metric {
        use super::*;
        use geo::{Contains, Geodesic, GeodesicArea, Length, Polygon, line_string, polygon};

        #[test]
        fn test_buffer_point_area() {
            let r = 100.0;
            let result = BufferTransform
                .apply(&fc(Geometry::Point(point!(x: 8.55, y: 47.37))), &params(r))
                .unwrap();

            let area = multipolygon(&result.features[0].geometry).geodesic_area_unsigned();
            let expected = PI * r * r;
            assert!(
                (area - expected).abs() / expected < 0.02,
                "point buffer area {area} not within 2% of {expected}"
            );
        }

        #[test]
        fn test_buffer_line_area() {
            let r = 100.0;
            let line = line_string![(x: 8.50, y: 47.37), (x: 8.52, y: 47.37)];
            let length = Geodesic.length(&line);
            let result = BufferTransform
                .apply(&fc(Geometry::LineString(line)), &params(r))
                .unwrap();

            let area = multipolygon(&result.features[0].geometry).geodesic_area_unsigned();
            let expected = 2.0 * r * length + PI * r * r;
            assert!(
                (area - expected).abs() / expected < 0.05,
                "line buffer area {area} not within 5% of {expected} (length {length})"
            );
        }

        #[test]
        fn test_buffer_polygon_grows_and_contains() {
            let r = 100.0;
            let original: Polygon<f64> = polygon![
                (x: 8.50, y: 47.36),
                (x: 8.52, y: 47.36),
                (x: 8.52, y: 47.38),
                (x: 8.50, y: 47.38),
            ];
            let orig_area = original.geodesic_area_unsigned();
            let perimeter = original.geodesic_perimeter();

            let result = BufferTransform
                .apply(&fc(Geometry::Polygon(original.clone())), &params(r))
                .unwrap();
            let buffered = multipolygon(&result.features[0].geometry);

            assert!(
                buffered.contains(&original),
                "buffered polygon must contain the original"
            );

            let area = buffered.geodesic_area_unsigned();
            let expected = orig_area + perimeter * r + PI * r * r;
            assert!(
                (area - expected).abs() / expected < 0.05,
                "polygon buffer area {area} not within 5% of {expected}"
            );
        }

        #[test]
        fn test_buffer_negative_shrinks_polygon() {
            let original: Polygon<f64> = polygon![
                (x: 8.50, y: 47.36),
                (x: 8.55, y: 47.36),
                (x: 8.55, y: 47.40),
                (x: 8.50, y: 47.40),
            ];
            let orig_area = original.geodesic_area_unsigned();

            let result = BufferTransform
                .apply(&fc(Geometry::Polygon(original)), &params(-100.0))
                .unwrap();
            let area = multipolygon(&result.features[0].geometry).geodesic_area_unsigned();

            assert!(
                area > 0.0 && area < orig_area,
                "negative buffer should shrink area: got {area}, original {orig_area}"
            );
        }
    }

    /// Without the reproject feature `distance` is CRS units, so buffering a
    /// lon/lat point by 1.0 gives roughly a unit circle in square degrees.
    #[cfg(not(feature = "reproject"))]
    #[test]
    fn test_buffer_without_reproject_uses_crs_units() {
        let result = BufferTransform
            .apply(
                &fc(Geometry::Point(point!(x: 8.55, y: 47.37))),
                &params(1.0),
            )
            .unwrap();

        let area = multipolygon(&result.features[0].geometry).unsigned_area();
        assert!(
            (area - PI).abs() / PI < 0.01,
            "expected a unit circle in square degrees, got {area}"
        );
    }

    #[test]
    fn test_buffer_roundtrips_through_registry() {
        use crate::registry::default_registry;

        let registry = default_registry();
        let op = registry.get("buffer").expect("buffer registered");
        let input = fc(Geometry::Point(point!(x: 0.0, y: 0.0)));

        let result = op.apply(&input, &params(5.0)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.crs, Some("EPSG:4326".into()));
        let mp = multipolygon(&result.features[0].geometry);
        assert!(mp.unsigned_area() > 0.0);
        // properties survive the transform
        assert_eq!(
            result.features[0].properties.get("id"),
            Some(&Value::Integer(1))
        );
    }

    #[test]
    fn test_buffer_point_is_multipolygon() {
        let result = BufferTransform
            .apply(&fc(Geometry::Point(point!(x: 0.0, y: 0.0))), &params(5.0))
            .unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(
            result.features[0].geometry,
            Geometry::MultiPolygon(_)
        ));
    }
}
