//! Geometry helpers over the topoi types a [`crate::feature::Feature`] carries.

pub use topoi_core::geojson::FeatureGeometry;
pub use topoi_core::{
    Coord, Envelope, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon, Ring,
};

/// GeoJSON type name of a geometry.
pub fn type_name(geom: &FeatureGeometry) -> &'static str {
    match geom {
        FeatureGeometry::Point(_) => "Point",
        FeatureGeometry::LineString(_) => "LineString",
        FeatureGeometry::Polygon(_) => "Polygon",
        FeatureGeometry::MultiPoint(_) => "MultiPoint",
        FeatureGeometry::MultiLineString(_) => "MultiLineString",
        FeatureGeometry::MultiPolygon(_) => "MultiPolygon",
        FeatureGeometry::GeometryCollection(_) => "GeometryCollection",
    }
}

/// Rebuild a geometry with every coordinate passed through `f`, keeping the variant.
pub fn map_coords<F>(geom: &FeatureGeometry, f: &F) -> FeatureGeometry
where
    F: Fn(Coord) -> Coord,
{
    match geom {
        FeatureGeometry::Point(p) => FeatureGeometry::Point(Point(f(p.0))),
        FeatureGeometry::LineString(ls) => FeatureGeometry::LineString(map_line(ls, f)),
        FeatureGeometry::Polygon(poly) => FeatureGeometry::Polygon(map_polygon(poly, f)),
        FeatureGeometry::MultiPoint(mp) => FeatureGeometry::MultiPoint(MultiPoint::new(
            mp.points().iter().map(|p| Point(f(p.0))).collect(),
        )),
        FeatureGeometry::MultiLineString(mls) => FeatureGeometry::MultiLineString(
            MultiLineString::new(mls.linestrings().iter().map(|ls| map_line(ls, f)).collect()),
        ),
        FeatureGeometry::MultiPolygon(mp) => FeatureGeometry::MultiPolygon(MultiPolygon::new(
            mp.polygons().iter().map(|p| map_polygon(p, f)).collect(),
        )),
        FeatureGeometry::GeometryCollection(members) => {
            FeatureGeometry::GeometryCollection(members.iter().map(|m| map_coords(m, f)).collect())
        }
    }
}

fn map_line<F>(line: &LineString, f: &F) -> LineString
where
    F: Fn(Coord) -> Coord,
{
    LineString::new(line.coords().iter().map(|c| f(*c)).collect())
}

fn map_ring<F>(ring: &Ring, f: &F) -> Ring
where
    F: Fn(Coord) -> Coord,
{
    Ring::new(ring.coords().iter().map(|c| f(*c)).collect())
}

fn map_polygon<F>(poly: &Polygon, f: &F) -> Polygon
where
    F: Fn(Coord) -> Coord,
{
    Polygon::new(
        map_ring(poly.exterior(), f),
        poly.interiors().iter().map(|r| map_ring(r, f)).collect(),
    )
}

/// Every coordinate of a geometry, in the order it is stored.
pub fn coords(geom: &FeatureGeometry) -> Vec<Coord> {
    let mut out = Vec::new();
    push_coords(geom, &mut out);
    out
}

fn push_coords(geom: &FeatureGeometry, out: &mut Vec<Coord>) {
    match geom {
        FeatureGeometry::Point(p) => out.push(p.0),
        FeatureGeometry::LineString(ls) => out.extend_from_slice(ls.coords()),
        FeatureGeometry::Polygon(poly) => push_polygon_coords(poly, out),
        FeatureGeometry::MultiPoint(mp) => out.extend(mp.points().iter().map(|p| p.0)),
        FeatureGeometry::MultiLineString(mls) => {
            for ls in mls.linestrings() {
                out.extend_from_slice(ls.coords());
            }
        }
        FeatureGeometry::MultiPolygon(mp) => {
            for poly in mp.polygons() {
                push_polygon_coords(poly, out);
            }
        }
        FeatureGeometry::GeometryCollection(members) => {
            for member in members {
                push_coords(member, out);
            }
        }
    }
}

fn push_polygon_coords(poly: &Polygon, out: &mut Vec<Coord>) {
    out.extend_from_slice(poly.exterior().coords());
    for hole in poly.interiors() {
        out.extend_from_slice(hole.coords());
    }
}

/// Bounding box of a geometry, or None when it holds no coordinates.
pub fn envelope(geom: &FeatureGeometry) -> Option<Envelope> {
    Envelope::from_coords(&coords(geom))
}

/// Structural equality. `FeatureGeometry` carries no `PartialEq`, so round trips
/// through a format compare with this.
pub fn equals(a: &FeatureGeometry, b: &FeatureGeometry) -> bool {
    match (a, b) {
        (FeatureGeometry::Point(x), FeatureGeometry::Point(y)) => x == y,
        (FeatureGeometry::LineString(x), FeatureGeometry::LineString(y)) => x == y,
        (FeatureGeometry::Polygon(x), FeatureGeometry::Polygon(y)) => x == y,
        (FeatureGeometry::MultiPoint(x), FeatureGeometry::MultiPoint(y)) => x == y,
        (FeatureGeometry::MultiLineString(x), FeatureGeometry::MultiLineString(y)) => x == y,
        (FeatureGeometry::MultiPolygon(x), FeatureGeometry::MultiPolygon(y)) => x == y,
        (FeatureGeometry::GeometryCollection(x), FeatureGeometry::GeometryCollection(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| equals(a, b))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polygon() -> FeatureGeometry {
        FeatureGeometry::Polygon(Polygon::new(
            Ring::new(vec![
                Coord::new(0.0, 0.0),
                Coord::new(4.0, 0.0),
                Coord::new(4.0, 4.0),
                Coord::new(0.0, 0.0),
            ]),
            vec![Ring::new(vec![
                Coord::new(1.0, 1.0),
                Coord::new(2.0, 1.0),
                Coord::new(2.0, 2.0),
                Coord::new(1.0, 1.0),
            ])],
        ))
    }

    #[test]
    fn test_map_coords_keeps_the_variant_and_holes() {
        let moved = map_coords(&polygon(), &|c| Coord::new(c.x + 1.0, c.y));
        match &moved {
            FeatureGeometry::Polygon(p) => {
                assert_eq!(p.exterior().coords()[0], Coord::new(1.0, 0.0));
                assert_eq!(p.interiors()[0].coords()[0], Coord::new(2.0, 1.0));
            }
            other => panic!("expected polygon, got {other:?}"),
        }
    }

    #[test]
    fn test_map_coords_recurses_into_a_collection() {
        let gc = FeatureGeometry::GeometryCollection(vec![
            FeatureGeometry::Point(Point::new(1.0, 1.0)),
            FeatureGeometry::GeometryCollection(vec![FeatureGeometry::MultiPoint(
                MultiPoint::new(vec![Point::new(2.0, 2.0)]),
            )]),
        ]);
        let doubled = map_coords(&gc, &|c| Coord::new(c.x * 2.0, c.y * 2.0));
        assert_eq!(
            coords(&doubled),
            vec![Coord::new(2.0, 2.0), Coord::new(4.0, 4.0)]
        );
    }

    #[test]
    fn test_envelope_covers_holes_and_members() {
        let env = envelope(&polygon()).unwrap();
        assert_eq!(env, Envelope::new(0.0, 0.0, 4.0, 4.0));
        assert!(envelope(&FeatureGeometry::GeometryCollection(vec![])).is_none());
    }

    #[test]
    fn test_equals_distinguishes_variants() {
        assert!(equals(&polygon(), &polygon()));
        let point = FeatureGeometry::Point(Point::new(0.0, 0.0));
        assert!(!equals(&point, &polygon()));
        assert!(equals(
            &FeatureGeometry::GeometryCollection(vec![point]),
            &FeatureGeometry::GeometryCollection(vec![FeatureGeometry::Point(Point::new(
                0.0, 0.0
            ))])
        ));
    }
}
