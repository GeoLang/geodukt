//! Shapefile reader/writer.
//!
//! A shapefile is really a set of sidecar files (.shp geometry, .shx index,
//! .dbf attributes, optional .prj CRS) that must agree with each other, and it
//! holds exactly one geometry type. Writes that would break either rule fail
//! instead of producing a file other tools would read differently.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use geodukt_core::feature::{Feature, FeatureCollection, Properties, Value};
use geodukt_core::pipeline::PipelineError;
use shapefile::dbase::{FieldName, FieldValue, Record, TableWriterBuilder};
use shapefile::record::EsriShape;

/// Read features from a Shapefile.
pub fn read_shapefile(path: &Path) -> Result<FeatureCollection, PipelineError> {
    let mut reader = shapefile::Reader::from_path(path).map_err(|e| PipelineError::Source {
        name: "shapefile".into(),
        message: format!("failed to open: {e}"),
    })?;

    let mut features = Vec::new();

    for result in reader.iter_shapes_and_records() {
        let (shape, record) = result.map_err(|e| PipelineError::Source {
            name: "shapefile".into(),
            message: e.to_string(),
        })?;

        let geometry = shape_to_geometry(&shape);
        let mut properties = HashMap::new();

        for (name, value) in record.into_iter() {
            let v = match value {
                shapefile::dbase::FieldValue::Character(Some(s)) => Value::String(s),
                shapefile::dbase::FieldValue::Numeric(Some(n)) => Value::Float(n),
                shapefile::dbase::FieldValue::Float(Some(n)) => Value::Float(n as f64),
                shapefile::dbase::FieldValue::Integer(n) => Value::Integer(n as i64),
                _ => Value::Null,
            };
            properties.insert(name, v);
        }

        features.push(Feature {
            geometry,
            properties,
        });
    }

    Ok(FeatureCollection::new(features, None))
}

fn shape_to_geometry(shape: &shapefile::Shape) -> geo::Geometry {
    match shape {
        shapefile::Shape::Point(p) => geo::Geometry::Point(geo::Point::new(p.x, p.y)),
        shapefile::Shape::Polyline(pl) => {
            let lines: Vec<geo::LineString> = pl
                .parts()
                .iter()
                .map(|part| {
                    geo::LineString::from(
                        part.iter()
                            .map(|p| geo::Coord { x: p.x, y: p.y })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            if lines.len() == 1 {
                geo::Geometry::LineString(lines.into_iter().next().unwrap())
            } else {
                geo::Geometry::MultiLineString(geo::MultiLineString::new(lines))
            }
        }
        shapefile::Shape::Polygon(pg) => {
            let mut exterior = None;
            let mut interiors = Vec::new();
            for ring in pg.rings() {
                let coords: Vec<geo::Coord> = match ring {
                    shapefile::PolygonRing::Outer(pts) | shapefile::PolygonRing::Inner(pts) => {
                        pts.iter().map(|p| geo::Coord { x: p.x, y: p.y }).collect()
                    }
                };
                let ls = geo::LineString::from(coords);
                if exterior.is_none() {
                    exterior = Some(ls);
                } else {
                    interiors.push(ls);
                }
            }
            if let Some(ext) = exterior {
                geo::Geometry::Polygon(geo::Polygon::new(ext, interiors))
            } else {
                geo::Geometry::Point(geo::Point::new(0.0, 0.0))
            }
        }
        shapefile::Shape::PointM(p) => geo::Geometry::Point(geo::Point::new(p.x, p.y)),
        shapefile::Shape::PointZ(p) => geo::Geometry::Point(geo::Point::new(p.x, p.y)),
        _ => geo::Geometry::Point(geo::Point::new(0.0, 0.0)),
    }
}

/// Attribute name limit of the .dbf table beside a shapefile.
const MAX_FIELD_NAME_LEN: usize = 10;

/// Maximum width of a .dbf field, in bytes.
const MAX_FIELD_LEN: usize = 254;

/// Decimal places used for float attributes. dBase stores numbers as fixed
/// point text, so this is the precision that survives a write.
const FLOAT_DECIMALS: u8 = 8;

fn sink_err(message: impl Into<String>) -> PipelineError {
    PipelineError::Sink {
        name: "shapefile".into(),
        message: message.into(),
    }
}

/// Write features to a Shapefile, creating the .shp, .shx and .dbf sidecars
/// next to `path` plus a .prj when the collection carries a known EPSG code.
///
/// Fails rather than mangling the data when the collection cannot be
/// represented: mixed geometry types, attribute names over 10 bytes, values
/// wider than a .dbf field, or geometries a shapefile has no shape for.
pub fn write_shapefile(path: &Path, fc: &FeatureCollection) -> Result<(), PipelineError> {
    if fc.features.is_empty() {
        return Err(sink_err(
            "cannot write an empty feature collection, a shapefile needs one geometry type",
        ));
    }

    let kind = single_shape_kind(fc)?;
    let fields = build_fields(fc)?;
    let builder = table_builder(&fields)?;

    crate::formats::create_parent_dir(path)?;

    match kind {
        ShapeKind::Point => write_shapes(path, builder, fc, &fields, to_point),
        ShapeKind::Multipoint => write_shapes(path, builder, fc, &fields, to_multipoint),
        ShapeKind::Polyline => write_shapes(path, builder, fc, &fields, to_polyline),
        ShapeKind::Polygon => write_shapes(path, builder, fc, &fields, to_polygon),
    }?;

    write_prj(path, fc.crs.as_deref())
}

/// The shapefile shape type a geometry maps onto. One shapefile holds exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapeKind {
    Point,
    Multipoint,
    Polyline,
    Polygon,
}

impl ShapeKind {
    fn label(self) -> &'static str {
        match self {
            ShapeKind::Point => "Point",
            ShapeKind::Multipoint => "MultiPoint",
            ShapeKind::Polyline => "PolyLine",
            ShapeKind::Polygon => "Polygon",
        }
    }

    fn of(geom: &geo::Geometry) -> Result<Self, PipelineError> {
        match geom {
            geo::Geometry::Point(_) => Ok(ShapeKind::Point),
            geo::Geometry::MultiPoint(_) => Ok(ShapeKind::Multipoint),
            // shapefile PolyLine and Polygon are multi-part, so the single and
            // multi geo variants land in the same file
            geo::Geometry::Line(_) | geo::Geometry::LineString(_) => Ok(ShapeKind::Polyline),
            geo::Geometry::MultiLineString(_) => Ok(ShapeKind::Polyline),
            geo::Geometry::Polygon(_) | geo::Geometry::MultiPolygon(_) => Ok(ShapeKind::Polygon),
            geo::Geometry::Rect(_) | geo::Geometry::Triangle(_) => Ok(ShapeKind::Polygon),
            geo::Geometry::GeometryCollection(_) => {
                Err(sink_err("a shapefile cannot hold a GeometryCollection"))
            }
        }
    }
}

fn single_shape_kind(fc: &FeatureCollection) -> Result<ShapeKind, PipelineError> {
    let kind = ShapeKind::of(&fc.features[0].geometry)?;
    for feature in &fc.features[1..] {
        let other = ShapeKind::of(&feature.geometry)?;
        if other != kind {
            return Err(sink_err(format!(
                "a shapefile holds one geometry type, found {} and {}",
                kind.label(),
                other.label()
            )));
        }
    }
    Ok(kind)
}

fn coords(ls: &geo::LineString) -> Vec<shapefile::Point> {
    ls.0.iter()
        .map(|c| shapefile::Point::new(c.x, c.y))
        .collect()
}

fn to_point(geom: &geo::Geometry) -> Result<shapefile::Point, PipelineError> {
    match geom {
        geo::Geometry::Point(p) => Ok(shapefile::Point::new(p.x(), p.y())),
        _ => Err(sink_err("expected a Point")),
    }
}

fn to_multipoint(geom: &geo::Geometry) -> Result<shapefile::Multipoint, PipelineError> {
    match geom {
        geo::Geometry::MultiPoint(mp) if !mp.0.is_empty() => Ok(shapefile::Multipoint::new(
            mp.0.iter()
                .map(|p| shapefile::Point::new(p.x(), p.y()))
                .collect(),
        )),
        geo::Geometry::MultiPoint(_) => Err(sink_err("cannot write an empty MultiPoint")),
        _ => Err(sink_err("expected a MultiPoint")),
    }
}

fn to_polyline(geom: &geo::Geometry) -> Result<shapefile::Polyline, PipelineError> {
    let parts: Vec<Vec<shapefile::Point>> = match geom {
        geo::Geometry::Line(l) => vec![vec![
            shapefile::Point::new(l.start.x, l.start.y),
            shapefile::Point::new(l.end.x, l.end.y),
        ]],
        geo::Geometry::LineString(ls) => vec![coords(ls)],
        geo::Geometry::MultiLineString(mls) => mls.0.iter().map(coords).collect(),
        _ => return Err(sink_err("expected a LineString or MultiLineString")),
    };

    if parts.is_empty() || parts.iter().any(|p| p.len() < 2) {
        return Err(sink_err(
            "every PolyLine part needs at least 2 points to be a valid shapefile record",
        ));
    }
    Ok(shapefile::Polyline::with_parts(parts))
}

fn to_polygon(geom: &geo::Geometry) -> Result<shapefile::Polygon, PipelineError> {
    let mut rings = Vec::new();
    match geom {
        geo::Geometry::Polygon(p) => push_polygon_rings(p, &mut rings),
        geo::Geometry::MultiPolygon(mp) => {
            for p in &mp.0 {
                push_polygon_rings(p, &mut rings);
            }
        }
        geo::Geometry::Rect(r) => push_polygon_rings(&r.to_polygon(), &mut rings),
        geo::Geometry::Triangle(t) => push_polygon_rings(&t.to_polygon(), &mut rings),
        _ => return Err(sink_err("expected a Polygon or MultiPolygon")),
    }

    if rings.is_empty() {
        return Err(sink_err("cannot write a Polygon with no rings"));
    }
    if rings.iter().any(|r| r.points().len() < 3) {
        return Err(sink_err(
            "every Polygon ring needs at least 3 points to be a valid shapefile record",
        ));
    }
    Ok(shapefile::Polygon::with_rings(rings))
}

fn push_polygon_rings(
    poly: &geo::Polygon,
    rings: &mut Vec<shapefile::PolygonRing<shapefile::Point>>,
) {
    rings.push(shapefile::PolygonRing::Outer(coords(poly.exterior())));
    for hole in poly.interiors() {
        rings.push(shapefile::PolygonRing::Inner(coords(hole)));
    }
}

/// A .dbf column: the property key it comes from and the column type it gets.
struct FieldSpec {
    key: String,
    kind: FieldKind,
}

enum FieldKind {
    Character(u8),
    Numeric { len: u8, decimals: u8 },
    Logical,
}

/// Derive the .dbf schema from every property seen in the collection. Columns
/// are sorted by name so the same input always produces the same table.
fn build_fields(fc: &FeatureCollection) -> Result<Vec<FieldSpec>, PipelineError> {
    let keys: BTreeSet<&String> = fc
        .features
        .iter()
        .flat_map(|f| f.properties.keys())
        .collect();

    keys.into_iter()
        .map(|key| {
            if key.len() > MAX_FIELD_NAME_LEN {
                return Err(sink_err(format!(
                    "attribute name '{key}' is {} bytes, a shapefile allows {MAX_FIELD_NAME_LEN}",
                    key.len()
                )));
            }
            let values: Vec<&Value> = fc
                .features
                .iter()
                .filter_map(|f| f.properties.get(key))
                .collect();
            Ok(FieldSpec {
                key: key.clone(),
                kind: field_kind(key, &values)?,
            })
        })
        .collect()
}

/// How a value goes into a text column. Shared with the width calculation, so a
/// column is always wide enough for what gets written into it.
fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(format_number(*f, FLOAT_DECIMALS)),
        Value::String(s) => Some(s.clone()),
    }
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// dBase stores numbers as fixed point text, and this is the form the writer
/// produces for a field with `decimals` decimal places.
fn format_number(value: f64, decimals: u8) -> String {
    format!("{value:.prec$}", prec = decimals as usize)
}

fn field_kind(key: &str, values: &[&Value]) -> Result<FieldKind, PipelineError> {
    let mut has_text = false;
    let mut has_bool = false;
    let mut has_number = false;
    let mut has_fraction = false;

    for value in values {
        match value {
            Value::Null => {}
            Value::Bool(_) => has_bool = true,
            Value::Integer(_) => has_number = true,
            Value::Float(_) => {
                has_number = true;
                has_fraction = true;
            }
            Value::String(_) => has_text = true,
        }
    }

    // one dBase type per column, so any mix of kinds falls back to text
    if has_bool && !has_text && !has_number {
        return Ok(FieldKind::Logical);
    }

    if has_number && !has_text && !has_bool {
        let decimals = if has_fraction { FLOAT_DECIMALS } else { 0 };
        let width = widest(values, |v| as_number(v).map(|n| format_number(n, decimals)));
        return Ok(FieldKind::Numeric {
            len: checked_width(key, width)?,
            decimals,
        });
    }

    Ok(FieldKind::Character(checked_width(
        key,
        widest(values, as_text),
    )?))
}

fn widest(values: &[&Value], render: impl Fn(&Value) -> Option<String>) -> usize {
    values
        .iter()
        .filter_map(|v| render(v))
        .map(|s| s.len())
        .max()
        .unwrap_or(1)
        .max(1)
}

fn checked_width(key: &str, width: usize) -> Result<u8, PipelineError> {
    if width > MAX_FIELD_LEN {
        return Err(sink_err(format!(
            "attribute '{key}' needs a {width} byte .dbf field, the limit is {MAX_FIELD_LEN}"
        )));
    }
    Ok(width as u8)
}

fn table_builder(fields: &[FieldSpec]) -> Result<TableWriterBuilder, PipelineError> {
    let mut builder = TableWriterBuilder::new();
    for field in fields {
        let name = FieldName::try_from(field.key.as_str())
            .map_err(|e| sink_err(format!("attribute name '{}': {e}", field.key)))?;
        builder = match field.kind {
            FieldKind::Character(len) => builder.add_character_field(name, len),
            FieldKind::Numeric { len, decimals } => builder.add_numeric_field(name, len, decimals),
            FieldKind::Logical => builder.add_logical_field(name),
        };
    }
    Ok(builder)
}

/// Build one .dbf record. Every column must be present, so properties missing
/// from a feature are written as the column's null value.
fn to_record(properties: &Properties, fields: &[FieldSpec]) -> Record {
    let mut record = Record::default();
    for field in fields {
        let value = properties.get(&field.key).unwrap_or(&Value::Null);
        let field_value = match field.kind {
            FieldKind::Logical => FieldValue::Logical(match value {
                Value::Bool(b) => Some(*b),
                _ => None,
            }),
            FieldKind::Numeric { .. } => FieldValue::Numeric(as_number(value)),
            FieldKind::Character(_) => FieldValue::Character(as_text(value)),
        };
        record.insert(field.key.clone(), field_value);
    }
    record
}

fn write_shapes<S, F>(
    path: &Path,
    builder: TableWriterBuilder,
    fc: &FeatureCollection,
    fields: &[FieldSpec],
    convert: F,
) -> Result<(), PipelineError>
where
    S: EsriShape,
    F: Fn(&geo::Geometry) -> Result<S, PipelineError>,
{
    let mut writer = shapefile::Writer::from_path(path, builder)
        .map_err(|e| sink_err(format!("failed to create {}: {e}", path.display())))?;

    for feature in &fc.features {
        let shape = convert(&feature.geometry)?;
        let record = to_record(&feature.properties, fields);
        writer
            .write_shape_and_record(&shape, &record)
            .map_err(|e| sink_err(e.to_string()))?;
    }
    Ok(())
}

/// Write the .prj sidecar when the CRS resolves to an EPSG code we have WKT for.
/// A missing CRS is not an error, the shapefile is still complete without it.
fn write_prj(path: &Path, crs: Option<&str>) -> Result<(), PipelineError> {
    let Some(wkt) = crs.and_then(epsg_code).and_then(crs_definitions::from_code) else {
        return Ok(());
    };
    std::fs::write(path.with_extension("prj"), wkt.wkt)?;
    Ok(())
}

fn epsg_code(crs: &str) -> Option<u16> {
    let (authority, code) = crs.split_once(':')?;
    if !authority.eq_ignore_ascii_case("EPSG") {
        return None;
    }
    code.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(geometry: geo::Geometry, props: &[(&str, Value)]) -> Feature {
        Feature {
            geometry,
            properties: props
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    fn point(x: f64, y: f64) -> geo::Geometry {
        geo::Geometry::Point(geo::Point::new(x, y))
    }

    fn square() -> geo::Geometry {
        geo::Geometry::Polygon(geo::Polygon::new(
            geo::LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]),
            vec![],
        ))
    }

    fn write_to_temp(fc: &FeatureCollection) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.shp");
        write_shapefile(&path, fc).unwrap();
        (dir, path)
    }

    #[test]
    fn test_point_roundtrip_keeps_attributes() {
        let fc = FeatureCollection::new(
            vec![
                feature(
                    point(1.0, 2.0),
                    &[
                        ("name", Value::String("park".into())),
                        ("visits", Value::Integer(42)),
                        ("area", Value::Float(1.5)),
                        ("open", Value::Bool(true)),
                    ],
                ),
                feature(
                    point(3.0, 4.0),
                    &[
                        ("name", Value::String("school".into())),
                        ("visits", Value::Integer(7)),
                        ("area", Value::Float(2.25)),
                        ("open", Value::Bool(false)),
                    ],
                ),
            ],
            Some("EPSG:4326".into()),
        );

        let (_dir, path) = write_to_temp(&fc);
        let back = read_shapefile(&path).unwrap();

        assert_eq!(back.len(), 2);
        assert_eq!(back.features[0].geometry, point(1.0, 2.0));
        assert_eq!(
            back.features[0].properties.get("name"),
            Some(&Value::String("park".into()))
        );
        assert_eq!(
            back.features[1].properties.get("visits"),
            Some(&Value::Float(7.0))
        );
        assert_eq!(
            back.features[0].properties.get("area"),
            Some(&Value::Float(1.5))
        );
    }

    #[test]
    fn test_writes_all_sidecars_and_prj() {
        let fc = FeatureCollection::new(
            vec![feature(point(1.0, 2.0), &[("id", Value::Integer(1))])],
            Some("EPSG:3857".into()),
        );
        let (_dir, path) = write_to_temp(&fc);

        for ext in ["shp", "shx", "dbf", "prj"] {
            assert!(path.with_extension(ext).exists(), "missing .{ext} sidecar");
        }
        let prj = std::fs::read_to_string(path.with_extension("prj")).unwrap();
        assert!(prj.contains("3857"), "prj should describe EPSG:3857: {prj}");
    }

    #[test]
    fn test_no_prj_without_crs() {
        let fc = FeatureCollection::new(
            vec![feature(point(0.0, 0.0), &[("id", Value::Integer(1))])],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        assert!(!path.with_extension("prj").exists());
    }

    #[test]
    fn test_polygon_roundtrip() {
        let fc =
            FeatureCollection::new(vec![feature(square(), &[("id", Value::Integer(1))])], None);
        let (_dir, path) = write_to_temp(&fc);
        let back = read_shapefile(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert!(matches!(
            back.features[0].geometry,
            geo::Geometry::Polygon(_)
        ));
    }

    #[test]
    fn test_polygon_and_multipolygon_share_one_file() {
        let multi = geo::Geometry::MultiPolygon(geo::MultiPolygon::new(vec![match square() {
            geo::Geometry::Polygon(p) => p,
            _ => unreachable!(),
        }]));
        let fc = FeatureCollection::new(
            vec![
                feature(square(), &[("id", Value::Integer(1))]),
                feature(multi, &[("id", Value::Integer(2))]),
            ],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        assert_eq!(read_shapefile(&path).unwrap().len(), 2);
    }

    #[test]
    fn test_mixed_geometry_types_rejected() {
        let fc = FeatureCollection::new(
            vec![
                feature(point(0.0, 0.0), &[("id", Value::Integer(1))]),
                feature(square(), &[("id", Value::Integer(2))]),
            ],
            None,
        );
        let dir = tempfile::tempdir().unwrap();
        let err = write_shapefile(&dir.path().join("mixed.shp"), &fc).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("one geometry type"), "{message}");
        assert!(
            message.contains("Point") && message.contains("Polygon"),
            "{message}"
        );
    }

    #[test]
    fn test_long_attribute_name_rejected() {
        let fc = FeatureCollection::new(
            vec![feature(
                point(0.0, 0.0),
                &[("population_density", Value::Integer(1))],
            )],
            None,
        );
        let dir = tempfile::tempdir().unwrap();
        let err = write_shapefile(&dir.path().join("long.shp"), &fc).unwrap_err();
        assert!(err.to_string().contains("population_density"), "{err}");
    }

    #[test]
    fn test_oversized_attribute_value_rejected() {
        let fc = FeatureCollection::new(
            vec![feature(
                point(0.0, 0.0),
                &[("note", Value::String("x".repeat(MAX_FIELD_LEN + 1)))],
            )],
            None,
        );
        let dir = tempfile::tempdir().unwrap();
        let err = write_shapefile(&dir.path().join("wide.shp"), &fc).unwrap_err();
        assert!(err.to_string().contains("the limit is 254"), "{err}");
    }

    #[test]
    fn test_empty_collection_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_shapefile(&dir.path().join("empty.shp"), &FeatureCollection::empty())
            .unwrap_err();
        assert!(
            err.to_string().contains("empty feature collection"),
            "{err}"
        );
    }

    #[test]
    fn test_geometry_collection_rejected() {
        let fc = FeatureCollection::new(
            vec![feature(
                geo::Geometry::GeometryCollection(geo::GeometryCollection::new_from(vec![point(
                    0.0, 0.0,
                )])),
                &[("id", Value::Integer(1))],
            )],
            None,
        );
        let dir = tempfile::tempdir().unwrap();
        let err = write_shapefile(&dir.path().join("gc.shp"), &fc).unwrap_err();
        assert!(err.to_string().contains("GeometryCollection"), "{err}");
    }

    #[test]
    fn test_degenerate_linestring_rejected() {
        let fc = FeatureCollection::new(
            vec![feature(
                geo::Geometry::LineString(geo::LineString::from(vec![(0.0, 0.0)])),
                &[("id", Value::Integer(1))],
            )],
            None,
        );
        let dir = tempfile::tempdir().unwrap();
        let err = write_shapefile(&dir.path().join("line.shp"), &fc).unwrap_err();
        assert!(err.to_string().contains("at least 2 points"), "{err}");
    }

    #[test]
    fn test_missing_property_written_as_null() {
        let fc = FeatureCollection::new(
            vec![
                feature(point(0.0, 0.0), &[("name", Value::String("a".into()))]),
                feature(point(1.0, 1.0), &[]),
            ],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        let back = read_shapefile(&path).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.features[1].properties.get("name"), Some(&Value::Null));
    }

    #[test]
    fn test_wide_integer_survives_a_float_column() {
        // the column gets 8 decimal places because of the float, so the integer
        // has to be sized with those decimals too or it gets truncated
        let fc = FeatureCollection::new(
            vec![
                feature(point(0.0, 0.0), &[("n", Value::Integer(123456789012345))]),
                feature(point(1.0, 1.0), &[("n", Value::Float(1.5))]),
            ],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        let back = read_shapefile(&path).unwrap();
        assert_eq!(
            back.features[0].properties.get("n"),
            Some(&Value::Float(123456789012345.0))
        );
        assert_eq!(
            back.features[1].properties.get("n"),
            Some(&Value::Float(1.5))
        );
    }

    #[test]
    fn test_bool_and_number_share_a_text_column() {
        let fc = FeatureCollection::new(
            vec![
                feature(point(0.0, 0.0), &[("mixed", Value::Bool(true))]),
                feature(point(1.0, 1.0), &[("mixed", Value::Integer(3))]),
            ],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        let back = read_shapefile(&path).unwrap();
        assert_eq!(
            back.features[0].properties.get("mixed"),
            Some(&Value::String("true".into()))
        );
        assert_eq!(
            back.features[1].properties.get("mixed"),
            Some(&Value::String("3".into()))
        );
    }

    #[test]
    fn test_epsg_code_parsing() {
        assert_eq!(epsg_code("EPSG:4326"), Some(4326));
        assert_eq!(epsg_code("epsg:3857"), Some(3857));
        assert_eq!(epsg_code("4326"), None);
        assert_eq!(epsg_code("ESRI:102100"), None);
    }
}
