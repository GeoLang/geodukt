//! CSV reader/writer — CSV files carry point geometry as lon/lat columns.
//!
//! The format has no place for a geometry type or a value type, so a round trip
//! through here has limits worth knowing:
//!
//! - only point geometry can be written, anything else is an error
//! - the reader lowercases headers, so mixed case property names come back lowercased
//! - the reader infers a value type per cell, so a string that looks like a
//!   number reads back as a number, `true`/`false` read back as strings, and an
//!   empty cell reads back as an empty string rather than null

use std::collections::{BTreeSet, HashMap};
use std::fs;

use geo::{Geometry, Point};
use geodukt_core::feature::{Feature, FeatureCollection, Properties, Value};
use geodukt_core::manifest::{Sink, Source};
use geodukt_core::pipeline::{PipelineError, SinkWriter, SourceReader};

/// Header of the column holding the point longitude.
const LON_COLUMN: &str = "lon";

/// Header of the column holding the point latitude.
const LAT_COLUMN: &str = "lat";

/// CSV source reader — expects columns named "lon"/"longitude" and "lat"/"latitude".
pub struct CsvReader;

impl SourceReader for CsvReader {
    fn read_source(&self, source: &Source) -> Result<FeatureCollection, PipelineError> {
        let path = &source.path;
        let content = fs::read_to_string(path).map_err(|e| PipelineError::Source {
            name: path.to_string(),
            message: e.to_string(),
        })?;

        let mut rdr = csv::Reader::from_reader(content.as_bytes());
        let headers: Vec<String> = rdr
            .headers()
            .map_err(|e| PipelineError::Source {
                name: path.to_string(),
                message: e.to_string(),
            })?
            .iter()
            .map(|h| h.to_lowercase())
            .collect();

        let lon_idx = headers
            .iter()
            .position(|h| h == "lon" || h == "longitude" || h == "x")
            .ok_or_else(|| PipelineError::Source {
                name: path.to_string(),
                message: "no lon/longitude/x column found".to_string(),
            })?;

        let lat_idx = headers
            .iter()
            .position(|h| h == "lat" || h == "latitude" || h == "y")
            .ok_or_else(|| PipelineError::Source {
                name: path.to_string(),
                message: "no lat/latitude/y column found".to_string(),
            })?;

        let mut features = Vec::new();
        for record in rdr.records() {
            let record = record.map_err(|e| PipelineError::Source {
                name: path.to_string(),
                message: e.to_string(),
            })?;

            let lon: f64 = record
                .get(lon_idx)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let lat: f64 = record
                .get(lat_idx)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);

            let mut props = HashMap::new();
            for (i, val) in record.iter().enumerate() {
                if i == lon_idx || i == lat_idx {
                    continue;
                }
                let key = headers
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("col_{i}"));
                let value = if let Ok(n) = val.parse::<i64>() {
                    Value::Integer(n)
                } else if let Ok(f) = val.parse::<f64>() {
                    Value::Float(f)
                } else {
                    Value::String(val.to_string())
                };
                props.insert(key, value);
            }

            features.push(Feature {
                geometry: Geometry::Point(Point::new(lon, lat)),
                properties: props,
            });
        }

        Ok(FeatureCollection::new(features, Some("EPSG:4326".into())))
    }
}

fn sink_err(path: &str, message: impl Into<String>) -> PipelineError {
    PipelineError::Sink {
        name: path.to_string(),
        message: message.into(),
    }
}

/// CSV sink writer — writes a `lon,lat` pair per feature followed by one column
/// per property, which is the layout [`CsvReader`] expects.
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

            let row = [point.x().to_string(), point.y().to_string()]
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

fn point_of(geometry: &Geometry) -> Option<Point> {
    match geometry {
        Geometry::Point(p) => Some(*p),
        _ => None,
    }
}

fn geometry_label(geometry: &Geometry) -> &'static str {
    match geometry {
        Geometry::Point(_) => "Point",
        Geometry::Line(_) => "Line",
        Geometry::LineString(_) => "LineString",
        Geometry::Polygon(_) => "Polygon",
        Geometry::MultiPoint(_) => "MultiPoint",
        Geometry::MultiLineString(_) => "MultiLineString",
        Geometry::MultiPolygon(_) => "MultiPolygon",
        Geometry::GeometryCollection(_) => "GeometryCollection",
        Geometry::Rect(_) => "Rect",
        Geometry::Triangle(_) => "Triangle",
    }
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
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_csv_reader() {
        let csv_data = "name,lon,lat\npark,1.5,2.5\nschool,-0.5,51.5\n";
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(csv_data.as_bytes()).unwrap();
        let path = tmp.path().to_str().unwrap();

        let fc = CsvReader
            .read_source(&Source {
                name: "points".into(),
                format: "csv".into(),
                path: path.to_string(),
                crs: None,
                layer: None,
            })
            .unwrap();
        assert_eq!(fc.len(), 2);
        assert_eq!(
            fc.features[0].properties.get("name"),
            Some(&Value::String("park".into()))
        );
    }

    fn source(path: &str) -> Source {
        Source {
            name: "in".into(),
            format: "csv".into(),
            path: path.to_string(),
            crs: None,
            layer: None,
        }
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
            geometry: Geometry::Point(Point::new(x, y)),
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
        let back = CsvReader
            .read_source(&source(path.to_str().unwrap()))
            .unwrap();

        assert_eq!(back.len(), 2);
        assert_eq!(back.crs.as_deref(), Some("EPSG:4326"));
        for (before, after) in fc.features.iter().zip(&back.features) {
            assert_eq!(before.geometry, after.geometry);
            assert_eq!(before.properties, after.properties);
        }
    }

    #[test]
    fn test_whole_float_stays_a_float() {
        // written as "2.0" so the reader does not infer an integer
        let fc = FeatureCollection::new(vec![feature(0.0, 0.0, &[("v", Value::Float(2.0))])], None);
        let (_dir, path) = write_to_temp(&fc);

        assert!(fs::read_to_string(&path).unwrap().contains("2.0"));
        let back = CsvReader
            .read_source(&source(path.to_str().unwrap()))
            .unwrap();
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

        let back = CsvReader
            .read_source(&source(path.to_str().unwrap()))
            .unwrap();
        assert_eq!(
            back.features[0].properties.get("name"),
            Some(&Value::String("Paris, France".into()))
        );
    }

    #[test]
    fn test_non_point_geometry_rejected() {
        let polygon = Geometry::Polygon(geo::Polygon::new(
            geo::LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]),
            vec![],
        ));
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
