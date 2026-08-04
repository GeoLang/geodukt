//! Dissolve transform — merge features by property value, unioning geometries.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::geometry::{FeatureGeometry, MultiPolygon, Polygon};
use geodukt_core::pipeline::{PipelineError, TransformOp};
use topoi_core::union;

/// Dissolve operation: groups features by a property key and unions their geometries.
pub struct DissolveTransform;

impl TransformOp for DissolveTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        let group_by = params
            .get("group_by")
            .and_then(|v: &toml::Value| v.as_str())
            .unwrap_or("");

        // Group features by property value
        let mut groups: HashMap<String, Vec<&Feature>> = HashMap::new();
        for f in &input.features {
            let key = if group_by.is_empty() {
                "__all__".to_string()
            } else {
                match f.properties.get(group_by) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Integer(n)) => n.to_string(),
                    Some(Value::Float(n)) => n.to_string(),
                    _ => "__null__".to_string(),
                }
            };
            groups.entry(key).or_default().push(f);
        }

        let features: Vec<Feature> = groups
            .into_iter()
            .filter_map(|(key, group)| {
                let merged = merge_geometries(&group)?;
                let mut props = HashMap::new();
                if !group_by.is_empty() {
                    props.insert(group_by.to_string(), Value::String(key));
                }
                props.insert("count".to_string(), Value::Integer(group.len() as i64));
                Some(Feature {
                    geometry: merged,
                    properties: props,
                })
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

fn merge_geometries(features: &[&Feature]) -> Option<FeatureGeometry> {
    let polys: Vec<&Polygon> = features
        .iter()
        .filter_map(|f| match &f.geometry {
            FeatureGeometry::Polygon(p) => Some(p),
            _ => None,
        })
        .collect();

    if polys.is_empty() {
        // For non-polygon features, just return a multi-point/line collection
        return features.first().map(|f| f.geometry.clone());
    }

    // Union all polygons
    let mut result = MultiPolygon::new(vec![polys[0].clone()]);
    for poly in polys.iter().skip(1) {
        result = union(&result, &MultiPolygon::new(vec![(*poly).clone()]));
    }

    match result.polygons() {
        [single] => Some(FeatureGeometry::Polygon(single.clone())),
        _ => Some(FeatureGeometry::MultiPolygon(result)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geodukt_core::geometry::{Coord, Ring};

    fn square(offset: f64) -> FeatureGeometry {
        FeatureGeometry::Polygon(Polygon::new(
            Ring::new(vec![
                Coord::new(offset, 0.0),
                Coord::new(offset + 1.0, 0.0),
                Coord::new(offset + 1.0, 1.0),
                Coord::new(offset, 1.0),
                Coord::new(offset, 0.0),
            ]),
            vec![],
        ))
    }

    #[test]
    fn test_dissolve_by_property() {
        let features = vec![
            Feature {
                geometry: square(0.0),
                properties: HashMap::from([("zone".into(), Value::String("A".into()))]),
            },
            Feature {
                geometry: square(1.0),
                properties: HashMap::from([("zone".into(), Value::String("A".into()))]),
            },
            Feature {
                geometry: FeatureGeometry::Polygon(Polygon::new(
                    Ring::new(vec![
                        Coord::new(5.0, 5.0),
                        Coord::new(6.0, 5.0),
                        Coord::new(6.0, 6.0),
                        Coord::new(5.0, 6.0),
                        Coord::new(5.0, 5.0),
                    ]),
                    vec![],
                )),
                properties: HashMap::from([("zone".into(), Value::String("B".into()))]),
            },
        ];
        let fc = FeatureCollection::new(features, None);
        let params = HashMap::from([("group_by".into(), toml::Value::String("zone".into()))]);

        let result = DissolveTransform.apply(&fc, &params).unwrap();
        assert_eq!(result.len(), 2);
    }
}
