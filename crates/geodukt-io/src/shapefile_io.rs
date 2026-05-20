//! Shapefile reader/writer.

use std::collections::HashMap;
use std::path::Path;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::pipeline::PipelineError;

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
