//! GeoPackage (SQLite) reader/writer.

use std::collections::HashMap;
use std::path::Path;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::geometry::{
    Coord, FeatureGeometry, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon,
    Ring,
};
use geodukt_core::pipeline::PipelineError;
use rusqlite::Connection;
use rusqlite::types::ValueRef;

/// Parse GeoPackage binary geometry (GP header + WKB).
fn parse_gpkg_geometry(data: &[u8]) -> FeatureGeometry {
    // GeoPackage binary format:
    // bytes 0-1: magic "GP"
    // byte 2: version
    // byte 3: flags (bits 1-3 = envelope type, bit 0 = byte order of header)
    // bytes 4-7: srs_id (int32)
    // then envelope (variable size based on flags), then WKB
    if data.len() < 8 || data[0] != b'G' || data[1] != b'P' {
        // Try as raw WKB
        return parse_wkb(data).unwrap_or_else(origin);
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
        return origin();
    }

    parse_wkb(&data[wkb_offset..]).unwrap_or_else(origin)
}

/// What an unreadable blob decodes to, so one bad row does not fail the read.
fn origin() -> FeatureGeometry {
    FeatureGeometry::Point(Point::new(0.0, 0.0))
}

fn parse_wkb(data: &[u8]) -> Option<FeatureGeometry> {
    Wkb::new(data).geometry()
}

/// Cursor over a WKB byte string. Every nested geometry carries its own byte
/// order byte, so the position and the endianness travel together.
struct Wkb<'a> {
    data: &'a [u8],
    pos: usize,
    le: bool,
}

impl<'a> Wkb<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            le: true,
        }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn count(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    fn number(&mut self) -> Option<f64> {
        let bytes: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(if self.le {
            f64::from_le_bytes(bytes)
        } else {
            f64::from_be_bytes(bytes)
        })
    }

    fn coord(&mut self) -> Option<Coord> {
        Some(Coord::new(self.number()?, self.number()?))
    }

    fn coords(&mut self) -> Option<Vec<Coord>> {
        let n = self.count()? as usize;
        (0..n).map(|_| self.coord()).collect()
    }

    fn polygon(&mut self) -> Option<Polygon> {
        let num_rings = self.count()? as usize;
        let mut rings: Vec<Ring> = Vec::with_capacity(num_rings);
        for _ in 0..num_rings {
            rings.push(Ring::new(self.coords()?));
        }
        if rings.is_empty() {
            return None;
        }
        Some(Polygon::new(rings.remove(0), rings))
    }

    fn members(&mut self) -> Option<Vec<FeatureGeometry>> {
        let n = self.count()? as usize;
        (0..n).map(|_| self.geometry()).collect()
    }

    /// One geometry, byte order and type header included.
    fn geometry(&mut self) -> Option<FeatureGeometry> {
        self.le = self.byte()? == 1;
        match self.count()? & 0xFF {
            1 => Some(FeatureGeometry::Point(Point(self.coord()?))),
            2 => Some(FeatureGeometry::LineString(LineString::new(self.coords()?))),
            3 => Some(FeatureGeometry::Polygon(self.polygon()?)),
            4 => {
                let points = self
                    .members()?
                    .into_iter()
                    .map(|g| match g {
                        FeatureGeometry::Point(p) => Some(p),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(FeatureGeometry::MultiPoint(MultiPoint::new(points)))
            }
            5 => {
                let lines = self
                    .members()?
                    .into_iter()
                    .map(|g| match g {
                        FeatureGeometry::LineString(ls) => Some(ls),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(FeatureGeometry::MultiLineString(MultiLineString::new(
                    lines,
                )))
            }
            6 => {
                let polygons = self
                    .members()?
                    .into_iter()
                    .map(|g| match g {
                        FeatureGeometry::Polygon(p) => Some(p),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(FeatureGeometry::MultiPolygon(MultiPolygon::new(polygons)))
            }
            7 => Some(FeatureGeometry::GeometryCollection(self.members()?)),
            _ => None,
        }
    }
}

/// A GeoPackage geometry blob: the GP header [`parse_gpkg_geometry`] documents,
/// with no envelope, followed by little-endian WKB.
fn gpkg_blob(geom: &FeatureGeometry, srs: i32) -> Vec<u8> {
    let mut out = vec![b'G', b'P', 0, 0b0000_0001];
    out.extend_from_slice(&srs.to_le_bytes());
    write_wkb(geom, &mut out);
    out
}

fn write_wkb(geom: &FeatureGeometry, out: &mut Vec<u8>) {
    let kind: u32 = match geom {
        FeatureGeometry::Point(_) => 1,
        FeatureGeometry::LineString(_) => 2,
        FeatureGeometry::Polygon(_) => 3,
        FeatureGeometry::MultiPoint(_) => 4,
        FeatureGeometry::MultiLineString(_) => 5,
        FeatureGeometry::MultiPolygon(_) => 6,
        FeatureGeometry::GeometryCollection(_) => 7,
    };
    out.push(1);
    out.extend_from_slice(&kind.to_le_bytes());

    match geom {
        FeatureGeometry::Point(p) => write_coord(p.0, out),
        FeatureGeometry::LineString(ls) => write_coords(ls.coords(), out),
        FeatureGeometry::Polygon(poly) => write_polygon(poly, out),
        FeatureGeometry::MultiPoint(mp) => {
            write_count(mp.points().len(), out);
            for p in mp.points() {
                write_wkb(&FeatureGeometry::Point(*p), out);
            }
        }
        FeatureGeometry::MultiLineString(mls) => {
            write_count(mls.linestrings().len(), out);
            for ls in mls.linestrings() {
                write_wkb(&FeatureGeometry::LineString(ls.clone()), out);
            }
        }
        FeatureGeometry::MultiPolygon(mp) => {
            write_count(mp.polygons().len(), out);
            for poly in mp.polygons() {
                write_wkb(&FeatureGeometry::Polygon(poly.clone()), out);
            }
        }
        FeatureGeometry::GeometryCollection(members) => {
            write_count(members.len(), out);
            for member in members {
                write_wkb(member, out);
            }
        }
    }
}

fn write_count(n: usize, out: &mut Vec<u8>) {
    out.extend_from_slice(&(n as u32).to_le_bytes());
}

fn write_coord(c: Coord, out: &mut Vec<u8>) {
    out.extend_from_slice(&c.x.to_le_bytes());
    out.extend_from_slice(&c.y.to_le_bytes());
}

fn write_coords(coords: &[Coord], out: &mut Vec<u8>) {
    write_count(coords.len(), out);
    for c in coords {
        write_coord(*c, out);
    }
}

fn write_polygon(poly: &Polygon, out: &mut Vec<u8>) {
    write_count(1 + poly.interiors().len(), out);
    write_coords(poly.exterior().coords(), out);
    for hole in poly.interiors() {
        write_coords(hole.coords(), out);
    }
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

fn sink_err(e: impl std::fmt::Display) -> PipelineError {
    PipelineError::Sink {
        name: "geopackage".into(),
        message: e.to_string(),
    }
}

/// SQLite column type for a property, chosen from every value seen for that key.
/// Mixed kinds fall back to TEXT, which SQLite accepts for anything.
fn column_type(fc: &FeatureCollection, key: &str) -> &'static str {
    let mut integer = false;
    let mut real = false;
    let mut other = false;

    for feature in &fc.features {
        match feature.properties.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::Integer(_)) | Some(Value::Bool(_)) => integer = true,
            Some(Value::Float(_)) => real = true,
            Some(Value::String(_)) => other = true,
        }
    }

    match (integer, real, other) {
        (_, _, true) => "TEXT",
        (true, true, false) | (false, true, false) => "REAL",
        (true, false, false) => "INTEGER",
        (false, false, false) => "TEXT",
    }
}

fn to_sql(value: Option<&Value>) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    match value {
        None | Some(Value::Null) => Sql::Null,
        Some(Value::Bool(b)) => Sql::Integer(*b as i64),
        Some(Value::Integer(i)) => Sql::Integer(*i),
        Some(Value::Float(f)) => Sql::Real(*f),
        Some(Value::String(s)) => Sql::Text(s.clone()),
    }
}

/// The gpkg_geometry_columns type name for the layer. A layer holding more than
/// one geometry type is declared as the generic GEOMETRY.
fn geometry_type_name(fc: &FeatureCollection) -> &'static str {
    let name_of = |g: &FeatureGeometry| match g {
        FeatureGeometry::Point(_) => "POINT",
        FeatureGeometry::LineString(_) => "LINESTRING",
        FeatureGeometry::Polygon(_) => "POLYGON",
        FeatureGeometry::MultiPoint(_) => "MULTIPOINT",
        FeatureGeometry::MultiLineString(_) => "MULTILINESTRING",
        FeatureGeometry::MultiPolygon(_) => "MULTIPOLYGON",
        FeatureGeometry::GeometryCollection(_) => "GEOMETRYCOLLECTION",
    };

    let mut kinds = fc.features.iter().map(|f| name_of(&f.geometry));
    match kinds.next() {
        Some(first) if kinds.all(|k| k == first) => first,
        _ => "GEOMETRY",
    }
}

/// EPSG code from a CRS string like `EPSG:4326`, defaulting to 4326 because
/// that is what the GeoJSON reader and the CSV reader produce.
fn srs_id(crs: Option<&str>) -> i32 {
    crs.and_then(|c| c.split_once(':'))
        .filter(|(authority, _)| authority.eq_ignore_ascii_case("EPSG"))
        .and_then(|(_, code)| code.trim().parse().ok())
        .unwrap_or(4326)
}

/// Write features to a GeoPackage file. Geometries go in as GeoPackage binary
/// (GP header plus WKB), attributes keep their type, and the layer is
/// registered in gpkg_contents so other GeoPackage readers find it.
///
/// The layer is replaced, so re-running a pipeline does not append a second copy
/// of the data. Other layers in the same file are left alone.
pub fn write_geopackage(
    path: &Path,
    fc: &FeatureCollection,
    table: &str,
) -> Result<(), PipelineError> {
    crate::formats::create_parent_dir(path).map_err(sink_err)?;
    let conn = Connection::open(path).map_err(|e| sink_err(format!("failed to open: {e}")))?;
    let srs = srs_id(fc.crs.as_deref());

    // GPKG magic in the SQLite application_id header field, so the file
    // identifies itself as a GeoPackage and not a bare SQLite database.
    // user_version is the spec version times 10000, GDAL warns without it.
    conn.pragma_update(None, "application_id", 0x4750_4B47i32)
        .map_err(sink_err)?;
    conn.pragma_update(None, "user_version", 10300i32)
        .map_err(sink_err)?;

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
        );
        CREATE TABLE IF NOT EXISTS gpkg_geometry_columns (
            table_name TEXT NOT NULL,
            column_name TEXT NOT NULL,
            geometry_type_name TEXT NOT NULL,
            srs_id INTEGER NOT NULL,
            z TINYINT NOT NULL,
            m TINYINT NOT NULL,
            CONSTRAINT pk_geom_cols PRIMARY KEY (table_name, column_name)
        );",
    )
    .map_err(sink_err)?;

    // the two rows the GeoPackage spec requires, plus the layer's own CRS
    conn.execute(
        "INSERT OR REPLACE INTO gpkg_spatial_ref_sys
            (srs_name, srs_id, organization, organization_coordsys_id, definition)
         VALUES ('Undefined cartesian', -1, 'NONE', -1, 'undefined'),
                ('Undefined geographic', 0, 'NONE', 0, 'undefined'),
                (?1, ?2, 'EPSG', ?2, ?3)",
        rusqlite::params![format!("EPSG:{srs}"), srs, crs_wkt(srs)],
    )
    .map_err(sink_err)?;

    // Attribute columns come from every feature, so a collection whose
    // features carry different keys does not lose the extra ones
    let mut columns: Vec<String> = fc
        .features
        .iter()
        .flat_map(|f| f.properties.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    columns.retain(|c| c != "fid" && c != "geom");

    let col_defs: String = columns
        .iter()
        .map(|c| format!(", \"{c}\" {}", column_type(fc, c)))
        .collect();

    // dropped first so a re-run replaces the layer instead of appending to it,
    // and so a changed attribute schema does not clash with the old columns
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS \"{table}\";
         CREATE TABLE \"{table}\" (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB{col_defs});"
    ))
    .map_err(sink_err)?;

    conn.execute(
        "INSERT OR REPLACE INTO gpkg_contents (table_name, data_type, identifier, srs_id)
         VALUES (?1, 'features', ?1, ?2)",
        rusqlite::params![table, srs],
    )
    .map_err(sink_err)?;

    conn.execute(
        "INSERT OR REPLACE INTO gpkg_geometry_columns
            (table_name, column_name, geometry_type_name, srs_id, z, m)
         VALUES (?1, 'geom', ?2, ?3, 0, 0)",
        rusqlite::params![table, geometry_type_name(fc), srs],
    )
    .map_err(sink_err)?;

    let placeholders: String = (2..columns.len() + 2)
        .map(|i| format!(", ?{i}"))
        .collect::<String>();
    let col_names: String = columns.iter().map(|c| format!(", \"{c}\"")).collect();
    let insert_sql = format!("INSERT INTO \"{table}\" (geom{col_names}) VALUES (?1{placeholders})");
    let mut stmt = conn.prepare(&insert_sql).map_err(sink_err)?;

    for feature in &fc.features {
        let blob = gpkg_blob(&feature.geometry, srs);

        let mut params: Vec<rusqlite::types::Value> = vec![rusqlite::types::Value::Blob(blob)];
        params.extend(columns.iter().map(|c| to_sql(feature.properties.get(c))));

        stmt.execute(rusqlite::params_from_iter(params))
            .map_err(sink_err)?;
    }

    Ok(())
}

fn crs_wkt(srs: i32) -> String {
    u16::try_from(srs)
        .ok()
        .and_then(crs_definitions::from_code)
        .map(|def| def.wkt.to_string())
        .unwrap_or_else(|| "undefined".to_string())
}
