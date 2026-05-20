//! GeoPackage (SQLite) reader/writer.

use std::collections::HashMap;
use std::path::Path;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::pipeline::PipelineError;
use rusqlite::Connection;

/// Parse GeoPackage binary geometry (GP header + WKB).
fn parse_gpkg_geometry(data: &[u8]) -> geo::Geometry {
    // GeoPackage binary format:
    // bytes 0-1: magic "GP"
    // byte 2: version
    // byte 3: flags (bits 1-3 = envelope type, bit 0 = byte order of header)
    // bytes 4-7: srs_id (int32)
    // then envelope (variable size based on flags), then WKB
    if data.len() < 8 || data[0] != b'G' || data[1] != b'P' {
        // Try as raw WKB
        return parse_wkb(data).unwrap_or_else(|| geo::Geometry::Point(geo::Point::new(0.0, 0.0)));
    }

    let flags = data[3];
    let envelope_type = (flags >> 1) & 0x07;
    let envelope_size = match envelope_type {
        0 => 0,
        1 => 32, // minx, maxx, miny, maxy
        2 => 48, // + minz, maxz
        3 => 48, // + minm, maxm
        4 => 64, // + minz, maxz, minm, maxm
        _ => 0,
    };

    let wkb_offset = 8 + envelope_size;
    if wkb_offset >= data.len() {
        return geo::Geometry::Point(geo::Point::new(0.0, 0.0));
    }

    parse_wkb(&data[wkb_offset..])
        .unwrap_or_else(|| geo::Geometry::Point(geo::Point::new(0.0, 0.0)))
}

/// Parse WKB geometry (limited to Point, LineString, Polygon, Multi* variants).
fn parse_wkb(data: &[u8]) -> Option<geo::Geometry> {
    if data.len() < 5 {
        return None;
    }

    let le = data[0] == 1;
    let geom_type = if le {
        u32::from_le_bytes([data[1], data[2], data[3], data[4]])
    } else {
        u32::from_be_bytes([data[1], data[2], data[3], data[4]])
    };

    let rest = &data[5..];

    match geom_type & 0xFF {
        1 => parse_wkb_point(rest, le).map(geo::Geometry::Point),
        2 => parse_wkb_linestring(rest, le).map(geo::Geometry::LineString),
        3 => parse_wkb_polygon(rest, le).map(geo::Geometry::Polygon),
        4 => parse_wkb_multi_point(rest, le).map(geo::Geometry::MultiPoint),
        5 => parse_wkb_multi_linestring(rest, le).map(geo::Geometry::MultiLineString),
        6 => parse_wkb_multi_polygon(rest, le).map(geo::Geometry::MultiPolygon),
        _ => None,
    }
}

fn read_f64(data: &[u8], offset: usize, le: bool) -> Option<f64> {
    if offset + 8 > data.len() {
        return None;
    }
    let bytes: [u8; 8] = data[offset..offset + 8].try_into().ok()?;
    Some(if le {
        f64::from_le_bytes(bytes)
    } else {
        f64::from_be_bytes(bytes)
    })
}

fn read_u32(data: &[u8], offset: usize, le: bool) -> Option<u32> {
    if offset + 4 > data.len() {
        return None;
    }
    let bytes: [u8; 4] = data[offset..offset + 4].try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn parse_wkb_point(data: &[u8], le: bool) -> Option<geo::Point> {
    let x = read_f64(data, 0, le)?;
    let y = read_f64(data, 8, le)?;
    Some(geo::Point::new(x, y))
}

fn parse_wkb_linestring(data: &[u8], le: bool) -> Option<geo::LineString> {
    let n = read_u32(data, 0, le)? as usize;
    let mut coords = Vec::with_capacity(n);
    for i in 0..n {
        let off = 4 + i * 16;
        let x = read_f64(data, off, le)?;
        let y = read_f64(data, off + 8, le)?;
        coords.push(geo::Coord { x, y });
    }
    Some(geo::LineString::from(coords))
}

fn parse_wkb_polygon(data: &[u8], le: bool) -> Option<geo::Polygon> {
    let num_rings = read_u32(data, 0, le)? as usize;
    let mut offset = 4;
    let mut rings = Vec::with_capacity(num_rings);
    for _ in 0..num_rings {
        let n = read_u32(data, offset, le)? as usize;
        offset += 4;
        let mut coords = Vec::with_capacity(n);
        for _ in 0..n {
            let x = read_f64(data, offset, le)?;
            let y = read_f64(data, offset + 8, le)?;
            coords.push(geo::Coord { x, y });
            offset += 16;
        }
        rings.push(geo::LineString::from(coords));
    }
    let exterior = rings.remove(0);
    Some(geo::Polygon::new(exterior, rings))
}

fn parse_wkb_multi_point(data: &[u8], le: bool) -> Option<geo::MultiPoint> {
    let n = read_u32(data, 0, le)? as usize;
    let mut points = Vec::with_capacity(n);
    let mut offset = 4;
    for _ in 0..n {
        // Each sub-geometry has its own WKB header (5 bytes)
        if offset + 5 + 16 > data.len() {
            return None;
        }
        let x = read_f64(data, offset + 5, le)?;
        let y = read_f64(data, offset + 13, le)?;
        points.push(geo::Point::new(x, y));
        offset += 5 + 16;
    }
    Some(geo::MultiPoint::new(points))
}

fn parse_wkb_multi_linestring(data: &[u8], le: bool) -> Option<geo::MultiLineString> {
    let n = read_u32(data, 0, le)? as usize;
    let mut lines = Vec::with_capacity(n);
    let mut offset = 4;
    for _ in 0..n {
        // skip 5-byte WKB header
        offset += 5;
        let num_pts = read_u32(data, offset, le)? as usize;
        offset += 4;
        let mut coords = Vec::with_capacity(num_pts);
        for _ in 0..num_pts {
            let x = read_f64(data, offset, le)?;
            let y = read_f64(data, offset + 8, le)?;
            coords.push(geo::Coord { x, y });
            offset += 16;
        }
        lines.push(geo::LineString::from(coords));
    }
    Some(geo::MultiLineString::new(lines))
}

fn parse_wkb_multi_polygon(data: &[u8], le: bool) -> Option<geo::MultiPolygon> {
    let n = read_u32(data, 0, le)? as usize;
    let mut polygons = Vec::with_capacity(n);
    let mut offset = 4;
    for _ in 0..n {
        // skip 5-byte WKB header
        offset += 5;
        let num_rings = read_u32(data, offset, le)? as usize;
        offset += 4;
        let mut rings = Vec::with_capacity(num_rings);
        for _ in 0..num_rings {
            let num_pts = read_u32(data, offset, le)? as usize;
            offset += 4;
            let mut coords = Vec::with_capacity(num_pts);
            for _ in 0..num_pts {
                let x = read_f64(data, offset, le)?;
                let y = read_f64(data, offset + 8, le)?;
                coords.push(geo::Coord { x, y });
                offset += 16;
            }
            rings.push(geo::LineString::from(coords));
        }
        if !rings.is_empty() {
            let exterior = rings.remove(0);
            polygons.push(geo::Polygon::new(exterior, rings));
        }
    }
    Some(geo::MultiPolygon::new(polygons))
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
        .prepare(&format!("PRAGMA table_info('{table_name}')"))
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
        .filter(|c| c != &geom_col && c != "fid")
        .collect();

    let col_list = format!(
        "\"{geom_col}\", {}",
        columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let query = format!("SELECT {col_list} FROM \"{table_name}\"");

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
                let val: String = row.get::<_, String>(i + 1).unwrap_or_default();
                props.insert(col.clone(), Value::String(val));
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

    Ok(FeatureCollection::new(features, None))
}

/// Write features to a GeoPackage file.
pub fn write_geopackage(
    path: &Path,
    fc: &FeatureCollection,
    table: &str,
) -> Result<(), PipelineError> {
    let conn = Connection::open(path).map_err(|e| PipelineError::Sink {
        name: "geopackage".into(),
        message: format!("failed to open: {e}"),
    })?;

    // Create GeoPackage metadata tables
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS gpkg_contents (
            table_name TEXT NOT NULL PRIMARY KEY,
            data_type TEXT NOT NULL,
            identifier TEXT,
            description TEXT DEFAULT '',
            last_change DATETIME DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE,
            srs_id INTEGER
        );
        CREATE TABLE IF NOT EXISTS gpkg_spatial_ref_sys (
            srs_name TEXT NOT NULL,
            srs_id INTEGER NOT NULL PRIMARY KEY,
            organization TEXT NOT NULL,
            organization_coordsys_id INTEGER NOT NULL,
            definition TEXT NOT NULL
        );",
    )
    .map_err(|e| PipelineError::Sink {
        name: "geopackage".into(),
        message: e.to_string(),
    })?;

    // Collect property columns from first feature
    let columns: Vec<String> = fc
        .features
        .first()
        .map(|f| f.properties.keys().cloned().collect())
        .unwrap_or_default();

    let col_defs: String = columns
        .iter()
        .map(|c| format!("\"{c}\" TEXT"))
        .collect::<Vec<_>>()
        .join(", ");

    conn.execute(
        &format!("CREATE TABLE IF NOT EXISTS \"{table}\" (fid INTEGER PRIMARY KEY AUTOINCREMENT, {col_defs})"),
        [],
    )
    .map_err(|e| PipelineError::Sink {
        name: "geopackage".into(),
        message: e.to_string(),
    })?;

    conn.execute(
        "INSERT OR REPLACE INTO gpkg_contents (table_name, data_type) VALUES (?1, 'features')",
        [table],
    )
    .map_err(|e| PipelineError::Sink {
        name: "geopackage".into(),
        message: e.to_string(),
    })?;

    let placeholders: String = columns.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let col_names: String = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql = format!("INSERT INTO \"{table}\" ({col_names}) VALUES ({placeholders})");

    for feature in &fc.features {
        let values: Vec<String> = columns
            .iter()
            .map(|c| match feature.properties.get(c) {
                Some(Value::String(s)) => s.clone(),
                Some(v) => format!("{v:?}"),
                None => String::new(),
            })
            .collect();

        let params: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        conn.execute(&insert_sql, params.as_slice())
            .map_err(|e| PipelineError::Sink {
                name: "geopackage".into(),
                message: e.to_string(),
            })?;
    }

    Ok(())
}
