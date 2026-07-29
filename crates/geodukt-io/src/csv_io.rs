//! CSV reader — reads CSV files with lon/lat columns.

use std::collections::HashMap;
use std::fs;

use geo::{Geometry, Point};
use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::manifest::Source;
use geodukt_core::pipeline::{PipelineError, SourceReader};

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
}
