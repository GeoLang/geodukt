//! GeoPackage (SQLite) reader/writer.

use std::collections::HashMap;
use std::path::Path;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::geometry::{FeatureGeometry, Point};
use geodukt_core::pipeline::PipelineError;
use geodukt_core::routing::to_json;
use geoplumb::VectorFeature;
use rusqlite::Connection;
use rusqlite::types::ValueRef;

use crate::formats::epsg_crs;

/// Parse GeoPackage binary geometry (GP header + WKB), or raw WKB without the
/// `GP` magic.
fn parse_gpkg_geometry(data: &[u8]) -> FeatureGeometry {
    let geometry = if data.len() < 8 || data[0] != b'G' || data[1] != b'P' {
        topoi_core::read_wkb(data)
    } else {
        topoi_core::read_geopackage_geometry(data)
    };
    geometry.unwrap_or_else(|_| origin())
}

/// What an unreadable blob decodes to, so one bad row does not fail the read.
fn origin() -> FeatureGeometry {
    FeatureGeometry::Point(Point::new(0.0, 0.0))
}

/// Read features from a GeoPackage file.
pub fn read_geopackage(
    path: &Path,
    table: Option<&str>,
) -> Result<FeatureCollection, PipelineError> {
    let conn = Connection::open(path).map_err(|e| PipelineError::Source {
        name: "geopackage".into(),
        message: format!("failed to open: {e}"),
    })?;

    // Find the first feature table if none specified
    let table_name = if let Some(t) = table {
        t.to_string()
    } else {
        conn.query_row(
            "SELECT table_name FROM gpkg_contents WHERE data_type='features' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| PipelineError::Source {
            name: "geopackage".into(),
            message: format!("no feature table found: {e}"),
        })?
    };
    let require_quotable = |identifier_kind: &str, name: &str| {
        require_double_quotable_identifier(identifier_kind, name).map_err(|message| {
            PipelineError::Source {
                name: "geopackage".into(),
                message,
            }
        })
    };
    require_quotable("layer name", &table_name)?;

    // Get geometry column name
    let geom_col: String = conn
        .query_row(
            "SELECT column_name FROM gpkg_geometry_columns WHERE table_name=?1 LIMIT 1",
            [&table_name],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "geom".to_string());

    // Get column info
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table_name}\")"))
        .map_err(|e| PipelineError::Source {
            name: "geopackage".into(),
            message: e.to_string(),
        })?;

    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| PipelineError::Source {
            name: "geopackage".into(),
            message: e.to_string(),
        })?
        .filter_map(|r| r.ok())
        .filter(|c| !c.eq_ignore_ascii_case(&geom_col) && !c.eq_ignore_ascii_case("fid"))
        .collect();

    require_quotable("geometry column", &geom_col)?;
    for column in &columns {
        require_quotable("column", column)?;
    }

    let col_list: String = columns.iter().map(|c| format!(", \"{c}\"")).collect();
    let query = format!("SELECT \"{geom_col}\"{col_list} FROM \"{table_name}\"");

    let mut stmt = conn.prepare(&query).map_err(|e| PipelineError::Source {
        name: "geopackage".into(),
        message: e.to_string(),
    })?;

    let features: Vec<Feature> = stmt
        .query_map([], |row| {
            let geom_data: Vec<u8> = row.get(0).unwrap_or_default();
            let geometry = parse_gpkg_geometry(&geom_data);

            let mut props = HashMap::new();
            for (i, col) in columns.iter().enumerate() {
                props.insert(col.clone(), sql_to_value(row.get_ref(i + 1)?));
            }
            Ok(Feature {
                geometry,
                properties: props,
            })
        })
        .map_err(|e| PipelineError::Source {
            name: "geopackage".into(),
            message: e.to_string(),
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(FeatureCollection::new(
        features,
        read_crs(&conn, &table_name),
    ))
}

/// Map a SQLite cell to a property value. GeoPackage stores attributes with the
/// declared column type, so this is how the write side's types come back.
fn sql_to_value(cell: ValueRef<'_>) -> Value {
    match cell {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
    }
}

/// Look up the layer's CRS as an `EPSG:<code>` string via gpkg_contents.
fn read_crs(conn: &Connection, table: &str) -> Option<String> {
    let srs_id: i64 = conn
        .query_row(
            "SELECT srs_id FROM gpkg_contents WHERE table_name=?1",
            [table],
            |row| row.get(0),
        )
        .ok()?;

    let (organization, code): (String, i64) = conn
        .query_row(
            "SELECT organization, organization_coordsys_id FROM gpkg_spatial_ref_sys WHERE srs_id=?1",
            [srs_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;

    (code > 0).then(|| format!("{}:{code}", organization.to_uppercase()))
}

// table and column names are formatted into SQL inside double quotes
fn require_double_quotable_identifier(identifier_kind: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{identifier_kind} cannot be empty"));
    }
    if name.contains('"') {
        return Err(format!(
            "{identifier_kind} '{name}' cannot contain a double quote, rename it"
        ));
    }
    if name.contains('\0') {
        return Err(format!(
            "{identifier_kind} {name:?} cannot contain a NUL character, rename it"
        ));
    }
    Ok(())
}

fn sink_err(e: impl std::fmt::Display) -> PipelineError {
    PipelineError::Sink {
        name: "geopackage".into(),
        message: e.to_string(),
    }
}

pub fn write_geopackage(
    path: &Path,
    fc: &FeatureCollection,
    table: &str,
) -> Result<(), PipelineError> {
    let crs = epsg_crs(&fc.crs).map_err(sink_err)?;
    let features = fc
        .features
        .iter()
        .enumerate()
        .map(|(id, feature)| vector_feature(id as u64, feature))
        .collect::<Result<Vec<_>, _>>()?;
    crate::formats::create_parent_dir(path).map_err(sink_err)?;
    geoplumb::elements::write_geopackage(path, table, &features, crs).map_err(sink_err)
}

fn vector_feature(id: u64, feature: &Feature) -> Result<VectorFeature, PipelineError> {
    let mut properties = HashMap::with_capacity(feature.properties.len());
    for (key, value) in &feature.properties {
        // geoplumb's write_geopackage names its primary key fid and its geometry column geom
        if key.eq_ignore_ascii_case("fid") || key.eq_ignore_ascii_case("geom") {
            continue;
        }
        let json = to_json(value).ok_or_else(|| {
            sink_err(format!(
                "property '{key}' is {value:?}, which a GeoPackage column cannot hold"
            ))
        })?;
        properties.insert(key.clone(), json);
    }
    Ok(VectorFeature {
        id,
        geometry: feature.geometry.clone(),
        properties,
    })
}
