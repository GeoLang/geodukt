//! GeoJSON reader/writer.

use std::collections::HashMap;
use std::fs;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::manifest::{Sink, Source};
use geodukt_core::pipeline::{PipelineError, SinkWriter, SourceReader};
use topoi_core::geojson;

/// GeoJSON source reader.
pub struct GeoJsonReader;

impl SourceReader for GeoJsonReader {
    fn read_source(&self, source: &Source) -> Result<FeatureCollection, PipelineError> {
        let path = &source.path;
        let content = fs::read_to_string(path).map_err(|e| PipelineError::Source {
            name: path.to_string(),
            message: e.to_string(),
        })?;

        let parsed = geojson::read_geojson(&content).map_err(|e| PipelineError::Source {
            name: path.to_string(),
            message: format!("invalid GeoJSON: {e}"),
        })?;

        let features = parsed
            .features
            .into_iter()
            .filter_map(|f| {
                Some(Feature {
                    geometry: f.geometry?,
                    properties: f
                        .properties
                        .iter()
                        .map(|(k, v)| (k.clone(), json_to_value(v)))
                        .collect(),
                })
            })
            .collect();

        Ok(FeatureCollection::new(features, Some("EPSG:4326".into())))
    }
}

/// GeoJSON sink writer.
pub struct GeoJsonWriter;

impl SinkWriter for GeoJsonWriter {
    fn write_sink(&self, data: &FeatureCollection, sink: &Sink) -> Result<(), PipelineError> {
        let path = &sink.path;
        let features: Vec<geojson::Feature> = data
            .features
            .iter()
            .map(|f| geojson::Feature {
                geometry: Some(f.geometry.clone()),
                properties: f
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), value_to_json(v)))
                    .collect::<HashMap<_, _>>(),
            })
            .collect();

        let fc = geojson::FeatureCollection { features };

        crate::formats::create_parent_dir(std::path::Path::new(path)).map_err(|e| {
            PipelineError::Sink {
                name: path.to_string(),
                message: e.to_string(),
            }
        })?;

        fs::write(path, geojson::write_geojson(&fc)).map_err(|e| PipelineError::Sink {
            name: path.to_string(),
            message: e.to_string(),
        })?;

        Ok(())
    }
}

fn json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        _ => Value::String(v.to_string()),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => {
            serde_json::Value::Number(serde_json::Number::from_f64(*f).unwrap_or(0.into()))
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_geojson_roundtrip() {
        let geojson_str = r#"{
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [1.0, 2.0]},
                "properties": {"name": "test"}
            }]
        }"#;

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(geojson_str.as_bytes()).unwrap();
        let path = tmp.path().to_str().unwrap();

        let fc = GeoJsonReader
            .read_source(&Source {
                name: "in".into(),
                format: "geojson".into(),
                path: path.to_string(),
                crs: None,
                layer: None,
            })
            .unwrap();
        assert_eq!(fc.len(), 1);

        let out = NamedTempFile::new().unwrap();
        let out_path = out.path().to_str().unwrap().to_string();
        GeoJsonWriter
            .write_sink(
                &fc,
                &Sink {
                    name: "out".into(),
                    input: "in".into(),
                    format: "geojson".into(),
                    path: out_path.clone(),
                    layer: None,
                },
            )
            .unwrap();

        let written = fs::read_to_string(&out_path).unwrap();
        assert!(written.contains("Point"));
    }
}
