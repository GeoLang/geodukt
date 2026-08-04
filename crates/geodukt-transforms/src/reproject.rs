//! Reproject transform — transform geometries between coordinate reference systems.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::geometry::{Coord, FeatureGeometry, map_coords};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use projicio_core::Transform;

/// Reproject operation: transforms coordinates from one CRS to another.
pub struct ReprojectTransform;

impl TransformOp for ReprojectTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let from_crs = params
            .get("from_crs")
            .and_then(|v: &toml::Value| v.as_str())
            .or(input.crs.as_deref())
            .unwrap_or("EPSG:4326");

        let to_crs = crate::params::string(params, "reproject", "to_crs")?;

        let transform = Transform::new(from_crs, to_crs).map_err(|e| PipelineError::Transform {
            name: "reproject".into(),
            message: format!("failed to create projection: {e}"),
        })?;

        let features: Vec<Feature> = input
            .features
            .iter()
            .map(|f| Feature {
                geometry: reproject_geometry(&f.geometry, &transform),
                properties: f.properties.clone(),
            })
            .collect();

        Ok(FeatureCollection::new(features, Some(to_crs.to_string())))
    }
}

fn reproject_geometry(geom: &FeatureGeometry, transform: &Transform) -> FeatureGeometry {
    map_coords(geom, &|c: Coord| {
        let (x, y) = transform.convert(c.x, c.y).unwrap_or((c.x, c.y));
        Coord::new(x, y)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::geometry::Point;

    #[test]
    fn test_reproject_4326_to_3857() {
        let features = vec![Feature {
            geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
            properties: HashMap::new(),
        }];
        let fc = FeatureCollection::new(features, Some("EPSG:4326".into()));
        let params = HashMap::from([
            ("from_crs".into(), toml::Value::String("EPSG:4326".into())),
            ("to_crs".into(), toml::Value::String("EPSG:3857".into())),
        ]);

        let result = ReprojectTransform.apply(&fc, &params).unwrap();
        assert_eq!(result.crs, Some("EPSG:3857".into()));
        assert_eq!(result.len(), 1);
        // Origin in 4326 maps to origin in 3857
        if let FeatureGeometry::Point(p) = &result.features[0].geometry {
            assert!(p.0.x.abs() < 1.0);
            assert!(p.0.y.abs() < 1.0);
        }
    }
}
