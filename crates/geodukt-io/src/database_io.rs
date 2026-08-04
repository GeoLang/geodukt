//! Enterprise database connectors — PostGIS, SQL Server, Oracle Spatial abstraction.
//!
//! Provides a unified interface for reading/writing geospatial features from
//! enterprise databases via SQL queries.

use serde::{Deserialize, Serialize};

use geodukt_core::feature::{FeatureCollection, Value};
use geodukt_core::geometry::FeatureGeometry;
use geodukt_core::pipeline::PipelineError;

/// Database backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseBackend {
    PostGIS,
    SqlServer,
    Oracle,
}

/// Database connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub backend: DatabaseBackend,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
    pub schema: Option<String>,
    pub ssl: bool,
}

impl DatabaseConfig {
    /// Build a connection string appropriate for the backend.
    pub fn connection_string(&self) -> String {
        let schema = self.schema.as_deref().unwrap_or("public");
        let ssl_mode = if self.ssl { "require" } else { "disable" };

        match self.backend {
            DatabaseBackend::PostGIS => {
                format!(
                    "host={} port={} dbname={} user={} password={} sslmode={} options=-csearch_path={}",
                    self.host,
                    self.port,
                    self.database,
                    self.username,
                    self.password,
                    ssl_mode,
                    schema
                )
            }
            DatabaseBackend::SqlServer => {
                format!(
                    "Server={},{};Database={};User Id={};Password={};Encrypt={};",
                    self.host, self.port, self.database, self.username, self.password, self.ssl
                )
            }
            DatabaseBackend::Oracle => {
                format!("//{}:{}/{}", self.host, self.port, self.database)
            }
        }
    }
}

/// A query specification for reading from a database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseQuery {
    /// SQL query or table name.
    pub query: String,
    /// Name of the geometry column.
    pub geometry_column: String,
    /// Spatial reference system ID.
    pub srid: Option<u32>,
    /// Maximum features to return (for preview/paging).
    pub limit: Option<usize>,
    /// Spatial extent filter (minx, miny, maxx, maxy).
    pub bbox: Option<[f64; 4]>,
}

impl DatabaseQuery {
    /// Build a SQL query with optional spatial filter and limit.
    pub fn build_sql(&self) -> String {
        let base = if self.query.trim().to_lowercase().starts_with("select") {
            self.query.clone()
        } else {
            format!("SELECT * FROM {}", self.query)
        };

        let mut sql = base;

        if let Some(bbox) = &self.bbox {
            let envelope = format!(
                "ST_MakeEnvelope({}, {}, {}, {}, {})",
                bbox[0],
                bbox[1],
                bbox[2],
                bbox[3],
                self.srid.unwrap_or(4326)
            );
            let where_clause = format!(
                " WHERE ST_Intersects({}, {})",
                self.geometry_column, envelope
            );
            if sql.to_lowercase().contains("where") {
                sql.push_str(&format!(
                    " AND ST_Intersects({}, {})",
                    self.geometry_column, envelope
                ));
            } else {
                sql.push_str(&where_clause);
            }
        }

        if let Some(limit) = self.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        sql
    }
}

/// Write specification for database output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseWriteConfig {
    pub table: String,
    pub geometry_column: String,
    pub srid: u32,
    pub mode: WriteMode,
}

/// How to handle existing data when writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    /// Append to existing table.
    Append,
    /// Drop and recreate table.
    Replace,
    /// Create table if not exists.
    CreateIfNotExists,
}

/// Trait for enterprise database connectors.
pub trait DatabaseConnector: Send + Sync {
    /// Test the connection.
    fn ping(&self) -> Result<(), PipelineError>;

    /// List available tables/layers.
    fn list_layers(&self) -> Result<Vec<String>, PipelineError>;

    /// Read features from a query.
    fn read_features(&self, query: &DatabaseQuery) -> Result<FeatureCollection, PipelineError>;

    /// Write features to a table.
    fn write_features(
        &self,
        config: &DatabaseWriteConfig,
        features: &FeatureCollection,
    ) -> Result<usize, PipelineError>;

    /// Get the row count for a table.
    fn count(&self, table: &str) -> Result<usize, PipelineError>;
}

/// Stub PostGIS connector (full implementation requires async postgres driver).
pub struct PostGisConnector {
    pub config: DatabaseConfig,
}

impl PostGisConnector {
    pub fn new(config: DatabaseConfig) -> Self {
        Self { config }
    }
}

impl DatabaseConnector for PostGisConnector {
    fn ping(&self) -> Result<(), PipelineError> {
        // In real impl: connect to postgres and run SELECT 1
        Ok(())
    }

    fn list_layers(&self) -> Result<Vec<String>, PipelineError> {
        // Would query geometry_columns view
        Ok(Vec::new())
    }

    fn read_features(&self, _query: &DatabaseQuery) -> Result<FeatureCollection, PipelineError> {
        // Would execute SQL query and parse WKB geometry
        Ok(FeatureCollection::empty())
    }

    fn write_features(
        &self,
        _config: &DatabaseWriteConfig,
        features: &FeatureCollection,
    ) -> Result<usize, PipelineError> {
        Ok(features.len())
    }

    fn count(&self, _table: &str) -> Result<usize, PipelineError> {
        Ok(0)
    }
}

/// Generate a CREATE TABLE DDL from a feature collection.
pub fn generate_create_table(
    table: &str,
    fc: &FeatureCollection,
    geom_col: &str,
    srid: u32,
) -> String {
    let mut columns = Vec::new();

    // Infer columns from first feature
    if let Some(feature) = fc.features.first() {
        for (key, value) in &feature.properties {
            let col_type = match value {
                Value::Null => "TEXT",
                Value::Bool(_) => "BOOLEAN",
                Value::Integer(_) => "BIGINT",
                Value::Float(_) => "DOUBLE PRECISION",
                Value::String(_) => "TEXT",
            };
            columns.push(format!("    {key} {col_type}"));
        }
    }

    let geom_type = fc
        .features
        .first()
        .map(|f| geometry_pg_type(&f.geometry))
        .unwrap_or("GEOMETRY");

    format!(
        "CREATE TABLE IF NOT EXISTS {table} (\n    id SERIAL PRIMARY KEY,\n{},\n    {geom_col} geometry({geom_type}, {srid})\n);",
        columns.join(",\n")
    )
}

fn geometry_pg_type(geom: &FeatureGeometry) -> &'static str {
    match geom {
        FeatureGeometry::Point(_) => "POINT",
        FeatureGeometry::MultiPoint(_) => "MULTIPOINT",
        FeatureGeometry::LineString(_) => "LINESTRING",
        FeatureGeometry::MultiLineString(_) => "MULTILINESTRING",
        FeatureGeometry::Polygon(_) => "POLYGON",
        FeatureGeometry::MultiPolygon(_) => "MULTIPOLYGON",
        _ => "GEOMETRY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::feature::Feature;
    use geodukt_core::geometry::Point;
    use std::collections::HashMap;

    #[test]
    fn test_connection_string_postgis() {
        let config = DatabaseConfig {
            backend: DatabaseBackend::PostGIS,
            host: "localhost".into(),
            port: 5432,
            database: "gis".into(),
            username: "user".into(),
            password: "pass".into(),
            schema: Some("public".into()),
            ssl: false,
        };
        let conn = config.connection_string();
        assert!(conn.contains("host=localhost"));
        assert!(conn.contains("port=5432"));
    }

    #[test]
    fn test_query_builder() {
        let query = DatabaseQuery {
            query: "buildings".into(),
            geometry_column: "geom".into(),
            srid: Some(4326),
            limit: Some(100),
            bbox: Some([0.0, 0.0, 10.0, 10.0]),
        };
        let sql = query.build_sql();
        assert!(sql.contains("SELECT * FROM buildings"));
        assert!(sql.contains("ST_Intersects"));
        assert!(sql.contains("LIMIT 100"));
    }

    #[test]
    fn test_query_builder_raw_sql() {
        let query = DatabaseQuery {
            query: "SELECT id, geom FROM parcels WHERE status = 'active'".into(),
            geometry_column: "geom".into(),
            srid: Some(4326),
            limit: None,
            bbox: None,
        };
        let sql = query.build_sql();
        assert!(sql.starts_with("SELECT id"));
    }

    #[test]
    fn test_generate_ddl() {
        let f = Feature {
            geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
            properties: HashMap::from([
                ("name".to_string(), Value::String("test".to_string())),
                ("pop".to_string(), Value::Integer(1000)),
            ]),
        };
        let fc = FeatureCollection::new(vec![f], None);
        let ddl = generate_create_table("cities", &fc, "geom", 4326);
        assert!(ddl.contains("CREATE TABLE IF NOT EXISTS cities"));
        assert!(ddl.contains("geometry(POINT, 4326)"));
    }

    #[test]
    fn test_postgis_connector() {
        let config = DatabaseConfig {
            backend: DatabaseBackend::PostGIS,
            host: "localhost".into(),
            port: 5432,
            database: "test".into(),
            username: "test".into(),
            password: "test".into(),
            schema: None,
            ssl: false,
        };
        let connector = PostGisConnector::new(config);
        assert!(connector.ping().is_ok());
    }
}
