//! Schema mapping transform — rename, drop, add, and cast property columns.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::pipeline::{PipelineError, TransformOp};

/// Schema map operation: rename/drop/add property columns.
pub struct SchemaMapTransform;

impl TransformOp for SchemaMapTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        crate::params::require_any(params, "schema_map")?;

        // Parse rename map: {"old_name": "new_name", ...}
        let renames: HashMap<String, String> = params
            .get("rename")
            .and_then(|v| v.as_table())
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        // Parse drop list
        let drops: Vec<String> = params
            .get("drop")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Parse add map: {"col_name": "default_value", ...}
        let adds: HashMap<String, Value> = params
            .get("add")
            .and_then(|v| v.as_table())
            .map(|t| {
                t.iter()
                    .map(|(k, v)| (k.clone(), toml_to_value(v)))
                    .collect()
            })
            .unwrap_or_default();

        let features: Vec<Feature> = input
            .features
            .iter()
            .map(|f| {
                let mut props = HashMap::new();

                // Copy and rename
                for (k, v) in &f.properties {
                    if drops.contains(k) {
                        continue;
                    }
                    let new_key = renames.get(k).cloned().unwrap_or_else(|| k.clone());
                    props.insert(new_key, v.clone());
                }

                // Add new columns
                for (k, v) in &adds {
                    props.entry(k.clone()).or_insert_with(|| v.clone());
                }

                Feature {
                    geometry: f.geometry.clone(),
                    properties: props,
                }
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }

    fn preserves_feature_order(&self) -> bool {
        true
    }
}

fn toml_to_value(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Integer(*i),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::geometry::{FeatureGeometry, Point};

    #[test]
    fn test_schema_map() {
        let features = vec![Feature {
            geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
            properties: HashMap::from([
                ("old_name".into(), Value::String("hello".into())),
                ("remove_me".into(), Value::Integer(42)),
            ]),
        }];
        let fc = FeatureCollection::new(features, None);

        let mut rename_table = toml::value::Table::new();
        rename_table.insert("old_name".into(), toml::Value::String("new_name".into()));

        let mut add_table = toml::value::Table::new();
        add_table.insert("source".into(), toml::Value::String("test".into()));

        let params = HashMap::from([
            ("rename".into(), toml::Value::Table(rename_table)),
            (
                "drop".into(),
                toml::Value::Array(vec![toml::Value::String("remove_me".into())]),
            ),
            ("add".into(), toml::Value::Table(add_table)),
        ]);

        let result = SchemaMapTransform.apply(&fc, &params).unwrap();
        let props = &result.features[0].properties;
        assert!(props.contains_key("new_name"));
        assert!(!props.contains_key("old_name"));
        assert!(!props.contains_key("remove_me"));
        assert_eq!(props.get("source"), Some(&Value::String("test".into())));
    }

    /// A schema_map with no rename, drop or add leaves the columns alone, which
    /// is never what the caller meant.
    #[test]
    fn test_schema_map_that_changes_nothing_fails_loud() {
        let fc = FeatureCollection::new(
            vec![Feature {
                geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
                properties: HashMap::new(),
            }],
            None,
        );
        let err = SchemaMapTransform.apply(&fc, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("at least one of"), "{err}");
    }
}
