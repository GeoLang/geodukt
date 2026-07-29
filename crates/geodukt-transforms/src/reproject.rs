//! Reproject transform — transform geometries between coordinate reference systems.

use std::collections::HashMap;

use geo::{Geometry, MapCoords};
use geodukt_core::feature::{Feature, FeatureCollection};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use proj::Proj;

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

        let proj =
            Proj::new_known_crs(from_crs, to_crs, None).map_err(|e| PipelineError::Transform {
                name: "reproject".into(),
                message: format!("failed to create projection: {e}"),
            })?;

        let features: Result<Vec<Feature>, PipelineError> = input
            .features
            .iter()
            .map(|f| {
                let reprojected = reproject_geometry(&f.geometry, &proj)?;
                Ok(Feature {
                    geometry: reprojected,
                    properties: f.properties.clone(),
                })
            })
            .collect();

        Ok(FeatureCollection::new(features?, Some(to_crs.to_string())))
    }
}

fn reproject_geometry(geom: &Geometry<f64>, proj: &Proj) -> Result<Geometry<f64>, PipelineError> {
    let result = geom.map_coords(|coord| {
        let (x, y) = proj
            .convert((coord.x, coord.y))
            .unwrap_or((coord.x, coord.y));
        geo::Coord { x, y }
    });
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::point;

    #[test]
    fn test_reproject_4326_to_3857() {
        let features = vec![Feature {
            geometry: Geometry::Point(point!(x: 0.0, y: 0.0)),
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
        if let Geometry::Point(p) = &result.features[0].geometry {
            assert!(p.x().abs() < 1.0);
            assert!(p.y().abs() < 1.0);
        }
    }
}
