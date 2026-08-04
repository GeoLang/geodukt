//! Schema auto-detection — infer field types from feature data.
//!
//! Analyzes sample features to determine column types, nullability,
//! and statistical properties for automatic schema generation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::feature::{FeatureCollection, Value};

/// Detected column type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    Boolean,
    Integer,
    Float,
    String,
    Null,
    Mixed,
}

/// Statistics for a detected column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnStats {
    pub col_type: ColumnType,
    pub nullable: bool,
    pub null_count: usize,
    pub distinct_count: usize,
    pub sample_count: usize,
    /// Min value (for numeric types).
    pub min: Option<f64>,
    /// Max value (for numeric types).
    pub max: Option<f64>,
    /// Max string length (for string types).
    pub max_length: Option<usize>,
}

/// A detected schema for a feature collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedSchema {
    pub columns: Vec<(String, ColumnStats)>,
    pub geometry_type: Option<String>,
    pub feature_count: usize,
    pub crs: Option<String>,
}

impl DetectedSchema {
    /// Detect schema from a feature collection.
    pub fn detect(fc: &FeatureCollection) -> Self {
        let mut column_values: HashMap<String, Vec<&Value>> = HashMap::new();

        for feature in &fc.features {
            for (key, value) in &feature.properties {
                column_values.entry(key.clone()).or_default().push(value);
            }
        }

        let feature_count = fc.features.len();
        let mut columns: Vec<(String, ColumnStats)> = column_values
            .into_iter()
            .map(|(name, values)| {
                let stats = analyze_column(&values, feature_count);
                (name, stats)
            })
            .collect();

        columns.sort_by(|a, b| a.0.cmp(&b.0));

        let geometry_type = fc.features.first().map(|f| geometry_type_name(&f.geometry));

        Self {
            columns,
            geometry_type,
            feature_count,
            crs: fc.crs.clone(),
        }
    }

    /// Generate a human-readable schema description.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Schema: {} features, geometry={}\n",
            self.feature_count,
            self.geometry_type.as_deref().unwrap_or("unknown")
        ));
        if let Some(crs) = &self.crs {
            out.push_str(&format!("CRS: {crs}\n"));
        }
        out.push_str("Columns:\n");
        for (name, stats) in &self.columns {
            let nullable = if stats.nullable { "?" } else { "" };
            out.push_str(&format!(
                "  {name}: {:?}{nullable} ({} samples, {} nulls)\n",
                stats.col_type, stats.sample_count, stats.null_count,
            ));
        }
        out
    }
}

fn analyze_column(values: &[&Value], total_features: usize) -> ColumnStats {
    let mut null_count = 0;
    let mut bool_count = 0;
    let mut int_count = 0;
    let mut float_count = 0;
    let mut string_count = 0;
    let mut min_val = f64::INFINITY;
    let mut max_val = f64::NEG_INFINITY;
    let mut max_length = 0usize;
    let mut distinct: Vec<String> = Vec::new();

    for value in values {
        match value {
            Value::Null => null_count += 1,
            Value::Bool(_) => bool_count += 1,
            Value::Integer(n) => {
                int_count += 1;
                let f = *n as f64;
                min_val = min_val.min(f);
                max_val = max_val.max(f);
            }
            Value::Float(f) => {
                float_count += 1;
                min_val = min_val.min(*f);
                max_val = max_val.max(*f);
            }
            Value::String(s) => {
                string_count += 1;
                max_length = max_length.max(s.len());
                if distinct.len() < 100 && !distinct.contains(s) {
                    distinct.push(s.clone());
                }
            }
        }
    }

    let non_null = values.len() - null_count;
    let col_type = if non_null == 0 {
        ColumnType::Null
    } else if int_count == non_null {
        ColumnType::Integer
    } else if float_count == non_null || (int_count + float_count) == non_null {
        ColumnType::Float
    } else if bool_count == non_null {
        ColumnType::Boolean
    } else if string_count == non_null {
        ColumnType::String
    } else {
        ColumnType::Mixed
    };

    let nullable = null_count > 0 || values.len() < total_features;

    ColumnStats {
        col_type,
        nullable,
        null_count,
        distinct_count: distinct.len(),
        sample_count: values.len(),
        min: if min_val.is_finite() {
            Some(min_val)
        } else {
            None
        },
        max: if max_val.is_finite() {
            Some(max_val)
        } else {
            None
        },
        max_length: if max_length > 0 {
            Some(max_length)
        } else {
            None
        },
    }
}

fn geometry_type_name(geom: &crate::geometry::FeatureGeometry) -> String {
    crate::geometry::type_name(geom).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Feature;
    use crate::geometry::{FeatureGeometry, Point};

    fn sample_fc() -> FeatureCollection {
        let features = vec![
            Feature {
                geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
                properties: HashMap::from([
                    ("name".to_string(), Value::String("foo".to_string())),
                    ("count".to_string(), Value::Integer(42)),
                    ("score".to_string(), Value::Float(0.95)),
                ]),
            },
            Feature {
                geometry: FeatureGeometry::Point(Point::new(3.0, 4.0)),
                properties: HashMap::from([
                    ("name".to_string(), Value::String("bar".to_string())),
                    ("count".to_string(), Value::Integer(7)),
                    ("score".to_string(), Value::Null),
                ]),
            },
        ];
        FeatureCollection::new(features, Some("EPSG:4326".to_string()))
    }

    #[test]
    fn test_detect_schema() {
        let schema = DetectedSchema::detect(&sample_fc());
        assert_eq!(schema.feature_count, 2);
        assert_eq!(schema.geometry_type, Some("Point".to_string()));
        assert_eq!(schema.columns.len(), 3);
    }

    #[test]
    fn test_column_types() {
        let schema = DetectedSchema::detect(&sample_fc());

        let count_col = schema.columns.iter().find(|(n, _)| n == "count").unwrap();
        assert_eq!(count_col.1.col_type, ColumnType::Integer);
        assert!(!count_col.1.nullable);
        assert_eq!(count_col.1.min, Some(7.0));
        assert_eq!(count_col.1.max, Some(42.0));

        let score_col = schema.columns.iter().find(|(n, _)| n == "score").unwrap();
        assert_eq!(score_col.1.col_type, ColumnType::Float);
        assert!(score_col.1.nullable);

        let name_col = schema.columns.iter().find(|(n, _)| n == "name").unwrap();
        assert_eq!(name_col.1.col_type, ColumnType::String);
    }

    #[test]
    fn test_describe() {
        let schema = DetectedSchema::detect(&sample_fc());
        let desc = schema.describe();
        assert!(desc.contains("2 features"));
        assert!(desc.contains("Point"));
        assert!(desc.contains("count"));
    }
}
