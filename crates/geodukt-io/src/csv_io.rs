//! CSV reader/writer — CSV files carry point geometry as lon/lat columns.
//!
//! The format has no place for a geometry type or a value type, so a round trip
//! through here has limits worth knowing:
//!
//! - only point geometry can be written, anything else is an error
//! - the reader infers a value type per cell, so a string that looks like a
//!   number reads back as a number, `true`/`false` read back as strings, and an
//!   empty cell reads back as an empty string rather than null

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use geodukt_core::feature::{FeatureCollection, Properties, Value};
use geodukt_core::geometry::{FeatureGeometry, Point};
use geodukt_core::manifest::Sink;
use geodukt_core::pipeline::{PipelineError, SinkWriter};

use crate::formats::epsg_crs;
use crate::geojson_io::features_with_geometry;

/// Header of the column holding the point longitude.
const LON_COLUMN: &str = "lon";

/// Header of the column holding the point latitude.
const LAT_COLUMN: &str = "lat";

pub fn read_csv(path: &Path, crs: &Option<String>) -> Result<FeatureCollection, PipelineError> {
    let source_err = |message: String| PipelineError::Source {
        name: path.display().to_string(),
        message,
    };
    let crs = epsg_crs(crs).map_err(source_err)?;
    let headers = csv::Reader::from_path(path)
        .and_then(|mut reader| reader.headers().cloned())
        .map_err(|e| source_err(e.to_string()))?;
    let column = |aliases: [&str; 3]| {
        headers
            .iter()
            .find(|header| aliases.contains(&header.to_lowercase().as_str()))
            .ok_or_else(|| source_err(format!("no {} column found", aliases.join("/"))))
    };
    let x_column = column([LON_COLUMN, "longitude", "x"])?;
    let y_column = column([LAT_COLUMN, "latitude", "y"])?;
    let collection = geoplumb::elements::read_csv(path, x_column, y_column)
        .map_err(|e| source_err(e.to_string()))?;
    Ok(FeatureCollection::new(
        features_with_geometry(collection),
        Some(crs.to_string()),
    ))
}

fn sink_err(path: &str, message: impl Into<String>) -> PipelineError {
    PipelineError::Sink {
        name: path.to_string(),
        message: message.into(),
    }
}

/// CSV sink writer — writes a `lon,lat` pair per feature followed by one column
/// per property, which is the layout [`read_csv`] expects.
pub struct CsvWriter;

impl SinkWriter for CsvWriter {
    fn write_sink(&self, data: &FeatureCollection, sink: &Sink) -> Result<(), PipelineError> {
        let path = &sink.path;
        let columns = attribute_columns(path, data)?;

        let mut writer = csv::Writer::from_writer(Vec::new());
        writer
            .write_record(
                [LON_COLUMN, LAT_COLUMN]
                    .into_iter()
                    .chain(columns.iter().map(String::as_str)),
            )
            .map_err(|e| sink_err(path, e.to_string()))?;

        for feature in &data.features {
            let point = point_of(&feature.geometry).ok_or_else(|| {
                sink_err(
                    path,
                    format!(
                        "csv carries point geometry as {LON_COLUMN}/{LAT_COLUMN} columns, cannot write a {}, use geojson, geopackage or shapefile instead",
                        geometry_label(&feature.geometry)
                    ),
                )
            })?;

            let row = [point.0.x.to_string(), point.0.y.to_string()]
                .into_iter()
                .chain(columns.iter().map(|c| cell(&feature.properties, c)));
            writer
                .write_record(row)
                .map_err(|e| sink_err(path, e.to_string()))?;
        }

        let content = writer
            .into_inner()
            .map_err(|e| sink_err(path, e.to_string()))?;

        crate::formats::create_parent_dir(std::path::Path::new(path))
            .map_err(|e| sink_err(path, e.to_string()))?;
        fs::write(path, content).map_err(|e| sink_err(path, e.to_string()))?;
        Ok(())
    }
}

/// Property columns in a stable order, rejecting names that would produce a
/// duplicate header and so read back as the wrong column.
fn attribute_columns(path: &str, data: &FeatureCollection) -> Result<Vec<String>, PipelineError> {
    let columns: Vec<String> = data
        .features
        .iter()
        .flat_map(|f| f.properties.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut seen = BTreeSet::from([LON_COLUMN.to_string(), LAT_COLUMN.to_string()]);
    for column in &columns {
        if !seen.insert(column.to_lowercase()) {
            return Err(sink_err(
                path,
                format!("property '{column}' clashes with another csv column name"),
            ));
        }
    }
    Ok(columns)
}

fn point_of(geometry: &FeatureGeometry) -> Option<Point> {
    match geometry {
        FeatureGeometry::Point(p) => Some(*p),
        _ => None,
    }
}

fn geometry_label(geometry: &FeatureGeometry) -> &'static str {
    geodukt_core::geometry::type_name(geometry)
}

/// Render one property. Floats keep a decimal point so the reader's type
/// inference does not turn a whole number back into an integer.
fn cell(properties: &Properties, column: &str) -> String {
    match properties.get(column) {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Integer(i)) => i.to_string(),
        Some(Value::Float(f)) => {
            let text = f.to_string();
            if text.parse::<i64>().is_ok() {
                format!("{text}.0")
            } else {
                text
            }
        }
        Some(Value::String(s)) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::feature::Feature;
    use geodukt_core::geometry::{Coord, Polygon};
    use std::collections::HashMap;

    fn csv_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.csv");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn test_coordinate_columns_resolve_by_alias_in_any_case() {
        let (_dir, path) = csv_file("Name,Latitude,X\npark,2.5,1.5\n");

        let fc = read_csv(&path, &None).unwrap();
        assert_eq!(fc.len(), 1);
        assert!(geodukt_core::geometry::equals(
            &fc.features[0].geometry,
            &FeatureGeometry::Point(Point::new(1.5, 2.5))
        ));
        assert_eq!(
            fc.features[0].properties,
            HashMap::from([("Name".to_string(), Value::String("park".into()))])
        );
    }

    #[test]
    fn test_crs_comes_from_the_source_and_defaults_to_wgs84() {
        let (_dir, path) = csv_file("lon,lat\n500000,4000000\n");

        let named = read_csv(&path, &Some("EPSG:32633".into())).unwrap();
        assert_eq!(named.crs.as_deref(), Some("EPSG:32633"));
        let unnamed = read_csv(&path, &None).unwrap();
        assert_eq!(unnamed.crs.as_deref(), Some("EPSG:4326"));
    }

    #[test]
    fn test_a_crs_that_is_not_an_epsg_code_is_refused() {
        let (_dir, path) = csv_file("lon,lat\n1,2\n");

        let error = read_csv(&path, &Some("urn:ogc:def:crs:OGC:1.3:CRS84".into()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not EPSG:<code>"), "{error}");
    }

    #[test]
    fn test_a_header_only_file_is_an_empty_collection() {
        let (_dir, path) = csv_file("lon,lat,name\n");

        let fc = read_csv(&path, &None).unwrap();
        assert!(fc.is_empty());
        assert_eq!(fc.crs.as_deref(), Some("EPSG:4326"));
    }

    #[test]
    fn test_a_coordinate_that_is_not_a_number_fails_naming_its_line() {
        let (_dir, path) = csv_file("lon,lat\n1,2\nnorth,3\n");

        let error = read_csv(&path, &None).unwrap_err().to_string();
        assert!(error.contains("line 3"), "{error}");
    }

    #[test]
    fn test_a_missing_coordinate_column_is_named() {
        let (_dir, path) = csv_file("name,lon\npark,1\n");

        let error = read_csv(&path, &None).unwrap_err().to_string();
        assert!(error.contains("no lat/latitude/y column"), "{error}");
    }

    fn sink(path: &str) -> Sink {
        Sink {
            name: "out".into(),
            input: "in".into(),
            format: "csv".into(),
            path: path.to_string(),
            layer: None,
        }
    }

    fn feature(x: f64, y: f64, props: &[(&str, Value)]) -> Feature {
        Feature {
            geometry: FeatureGeometry::Point(Point::new(x, y)),
            properties: props
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    fn write_to_temp(fc: &FeatureCollection) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        CsvWriter
            .write_sink(fc, &sink(path.to_str().unwrap()))
            .unwrap();
        (dir, path)
    }

    #[test]
    fn test_csv_roundtrip_preserves_points_and_attributes() {
        let fc = FeatureCollection::new(
            vec![
                feature(
                    1.5,
                    2.5,
                    &[
                        ("name", Value::String("park".into())),
                        ("visits", Value::Integer(42)),
                        ("area", Value::Float(1.25)),
                    ],
                ),
                feature(
                    -0.5,
                    51.5,
                    &[
                        ("name", Value::String("school".into())),
                        ("visits", Value::Integer(-7)),
                        ("area", Value::Float(2.0)),
                    ],
                ),
            ],
            Some("EPSG:4326".into()),
        );

        let (_dir, path) = write_to_temp(&fc);
        let back = read_csv(&path, &None).unwrap();

        assert_eq!(back.len(), 2);
        assert_eq!(back.crs.as_deref(), Some("EPSG:4326"));
        for (before, after) in fc.features.iter().zip(&back.features) {
            assert!(geodukt_core::geometry::equals(
                &before.geometry,
                &after.geometry
            ));
            assert_eq!(before.properties, after.properties);
        }
    }

    #[test]
    fn test_whole_float_stays_a_float() {
        // written as "2.0" so the reader does not infer an integer
        let fc = FeatureCollection::new(vec![feature(0.0, 0.0, &[("v", Value::Float(2.0))])], None);
        let (_dir, path) = write_to_temp(&fc);

        assert!(fs::read_to_string(&path).unwrap().contains("2.0"));
        let back = read_csv(&path, &None).unwrap();
        assert_eq!(
            back.features[0].properties.get("v"),
            Some(&Value::Float(2.0))
        );
    }

    #[test]
    fn test_header_is_lon_lat_then_sorted_properties() {
        let fc = FeatureCollection::new(
            vec![feature(
                3.0,
                4.0,
                &[
                    ("zone", Value::String("a".into())),
                    ("id", Value::Integer(1)),
                ],
            )],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().next(), Some("lon,lat,id,zone"));
        assert_eq!(written.lines().nth(1), Some("3,4,1,a"));
    }

    #[test]
    fn test_missing_property_written_as_empty_cell() {
        let fc = FeatureCollection::new(
            vec![
                feature(0.0, 0.0, &[("name", Value::String("a".into()))]),
                feature(1.0, 1.0, &[]),
            ],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        assert_eq!(
            fs::read_to_string(&path).unwrap().lines().nth(2),
            Some("1,1,")
        );
    }

    #[test]
    fn test_quotes_a_value_holding_a_comma() {
        let fc = FeatureCollection::new(
            vec![feature(
                0.0,
                0.0,
                &[("name", Value::String("Paris, France".into()))],
            )],
            None,
        );
        let (_dir, path) = write_to_temp(&fc);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("\"Paris, France\"")
        );

        let back = read_csv(&path, &None).unwrap();
        assert_eq!(
            back.features[0].properties.get("name"),
            Some(&Value::String("Paris, France".into()))
        );
    }

    #[test]
    fn test_non_point_geometry_rejected() {
        let polygon = FeatureGeometry::Polygon(Polygon::from_coords(&[
            Coord::new(0.0, 0.0),
            Coord::new(1.0, 0.0),
            Coord::new(1.0, 1.0),
        ]));
        let fc = FeatureCollection::new(
            vec![Feature {
                geometry: polygon,
                properties: HashMap::new(),
            }],
            None,
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.csv");
        let err = CsvWriter
            .write_sink(&fc, &sink(path.to_str().unwrap()))
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("cannot write a Polygon"), "{message}");
        assert!(message.contains("geojson"), "{message}");
    }

    #[test]
    fn test_property_clashing_with_geometry_column_rejected() {
        let fc =
            FeatureCollection::new(vec![feature(0.0, 0.0, &[("Lon", Value::Float(9.0))])], None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clash.csv");
        let err = CsvWriter
            .write_sink(&fc, &sink(path.to_str().unwrap()))
            .unwrap_err();
        assert!(err.to_string().contains("clashes"), "{err}");
    }
}
