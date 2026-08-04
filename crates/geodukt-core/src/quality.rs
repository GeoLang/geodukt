//! Data quality rules — validate geometry and attribute constraints.

use std::collections::HashMap;

use crate::feature::{FeatureCollection, Value};
use crate::geometry::FeatureGeometry;

/// A quality rule that can be applied to a feature collection.
#[derive(Debug, Clone)]
pub enum QualityRule {
    /// All geometries must be valid (non-zero area for polygons, non-empty for lines).
    GeometryValid,
    /// A property must not be null.
    NotNull(String),
    /// A property must be unique across all features.
    Unique(String),
    /// Feature count must be at least N.
    MinCount(usize),
    /// Feature count must be at most N.
    MaxCount(usize),
    /// A property must match a specific value.
    PropertyEquals(String, Value),
    /// Geometry type must match.
    GeometryType(String),
    /// Polygon area must be above threshold.
    MinArea(f64),
    /// Custom predicate with description.
    Custom { name: String, description: String },
}

/// Result of running a quality check.
#[derive(Debug, Clone)]
pub struct QualityResult {
    pub rule: String,
    pub passed: bool,
    pub message: String,
    pub failing_indices: Vec<usize>,
}

/// Run all quality rules against a feature collection.
pub fn check_quality(fc: &FeatureCollection, rules: &[QualityRule]) -> Vec<QualityResult> {
    rules.iter().map(|rule| apply_rule(fc, rule)).collect()
}

fn apply_rule(fc: &FeatureCollection, rule: &QualityRule) -> QualityResult {
    match rule {
        QualityRule::GeometryValid => {
            let mut failing = Vec::new();
            for (i, f) in fc.features.iter().enumerate() {
                if !is_geometry_valid(&f.geometry) {
                    failing.push(i);
                }
            }
            QualityResult {
                rule: "geometry_valid".into(),
                passed: failing.is_empty(),
                message: if failing.is_empty() {
                    "All geometries valid".into()
                } else {
                    format!("{} invalid geometries", failing.len())
                },
                failing_indices: failing,
            }
        }
        QualityRule::NotNull(field) => {
            let mut failing = Vec::new();
            for (i, f) in fc.features.iter().enumerate() {
                match f.properties.get(field) {
                    None | Some(Value::Null) => failing.push(i),
                    _ => {}
                }
            }
            QualityResult {
                rule: format!("not_null({field})"),
                passed: failing.is_empty(),
                message: if failing.is_empty() {
                    format!("All features have non-null '{field}'")
                } else {
                    format!("{} features have null '{field}'", failing.len())
                },
                failing_indices: failing,
            }
        }
        QualityRule::Unique(field) => {
            let mut seen: HashMap<String, usize> = HashMap::new();
            let mut failing = Vec::new();
            for (i, f) in fc.features.iter().enumerate() {
                let key = match f.properties.get(field) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Integer(n)) => n.to_string(),
                    Some(v) => format!("{v:?}"),
                    None => "__null__".into(),
                };
                if let Some(&prev) = seen.get(&key) {
                    failing.push(i);
                    if !failing.contains(&prev) {
                        failing.push(prev);
                    }
                } else {
                    seen.insert(key, i);
                }
            }
            QualityResult {
                rule: format!("unique({field})"),
                passed: failing.is_empty(),
                message: if failing.is_empty() {
                    format!("All '{field}' values are unique")
                } else {
                    format!("{} duplicate '{field}' values", failing.len())
                },
                failing_indices: failing,
            }
        }
        QualityRule::MinCount(n) => QualityResult {
            rule: format!("min_count({n})"),
            passed: fc.len() >= *n,
            message: format!("Feature count: {} (min: {n})", fc.len()),
            failing_indices: vec![],
        },
        QualityRule::MaxCount(n) => QualityResult {
            rule: format!("max_count({n})"),
            passed: fc.len() <= *n,
            message: format!("Feature count: {} (max: {n})", fc.len()),
            failing_indices: vec![],
        },
        QualityRule::GeometryType(expected) => {
            let mut failing = Vec::new();
            for (i, f) in fc.features.iter().enumerate() {
                let gtype = geometry_type_name(&f.geometry);
                if gtype != expected {
                    failing.push(i);
                }
            }
            QualityResult {
                rule: format!("geometry_type({expected})"),
                passed: failing.is_empty(),
                message: if failing.is_empty() {
                    format!("All geometries are {expected}")
                } else {
                    format!("{} features have wrong geometry type", failing.len())
                },
                failing_indices: failing,
            }
        }
        QualityRule::MinArea(threshold) => {
            let mut failing = Vec::new();
            for (i, f) in fc.features.iter().enumerate() {
                let area = match &f.geometry {
                    FeatureGeometry::Polygon(p) => p.area(),
                    FeatureGeometry::MultiPolygon(mp) => mp.area(),
                    _ => 0.0,
                };
                if area < *threshold {
                    failing.push(i);
                }
            }
            QualityResult {
                rule: format!("min_area({threshold})"),
                passed: failing.is_empty(),
                message: if failing.is_empty() {
                    "All polygon areas above threshold".into()
                } else {
                    format!("{} polygons below min area", failing.len())
                },
                failing_indices: failing,
            }
        }
        QualityRule::PropertyEquals(field, expected) => {
            let mut failing = Vec::new();
            for (i, f) in fc.features.iter().enumerate() {
                if f.properties.get(field) != Some(expected) {
                    failing.push(i);
                }
            }
            QualityResult {
                rule: format!("property_equals({field})"),
                passed: failing.is_empty(),
                message: if failing.is_empty() {
                    format!("All '{field}' values match expected")
                } else {
                    format!("{} features don't match", failing.len())
                },
                failing_indices: failing,
            }
        }
        QualityRule::Custom { name, description } => QualityResult {
            rule: name.clone(),
            passed: true,
            message: description.clone(),
            failing_indices: vec![],
        },
    }
}

fn is_geometry_valid(geom: &FeatureGeometry) -> bool {
    match geom {
        FeatureGeometry::Polygon(p) => p.area() > 0.0 && p.exterior().coords().len() >= 4,
        FeatureGeometry::LineString(ls) => ls.coords().len() >= 2,
        FeatureGeometry::Point(_) => true,
        FeatureGeometry::MultiPolygon(mp) => mp.polygons().iter().all(|p| p.area() > 0.0),
        FeatureGeometry::MultiLineString(mls) => {
            mls.linestrings().iter().all(|ls| ls.coords().len() >= 2)
        }
        FeatureGeometry::MultiPoint(mp) => !mp.points().is_empty(),
        FeatureGeometry::GeometryCollection(_) => true,
    }
}

fn geometry_type_name(geom: &FeatureGeometry) -> &'static str {
    match geom {
        FeatureGeometry::GeometryCollection(_) => "Unknown",
        other => crate::geometry::type_name(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Feature;
    use crate::geometry::{Coord, Point, Polygon, Ring};

    #[test]
    fn test_quality_rules() {
        let features = vec![
            Feature {
                geometry: FeatureGeometry::Polygon(Polygon::new(
                    Ring::new(vec![
                        Coord::new(0.0, 0.0),
                        Coord::new(1.0, 0.0),
                        Coord::new(1.0, 1.0),
                        Coord::new(0.0, 1.0),
                        Coord::new(0.0, 0.0),
                    ]),
                    vec![],
                )),
                properties: HashMap::from([("id".into(), Value::Integer(1))]),
            },
            Feature {
                geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
                properties: HashMap::from([("id".into(), Value::Integer(2))]),
            },
        ];
        let fc = FeatureCollection::new(features, None);

        let rules = vec![
            QualityRule::NotNull("id".into()),
            QualityRule::MinCount(1),
            QualityRule::MaxCount(10),
            QualityRule::Unique("id".into()),
        ];

        let results = check_quality(&fc, &rules);
        assert!(results.iter().all(|r| r.passed));
    }

    #[test]
    fn test_geometry_type_check() {
        let features = vec![Feature {
            geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
            properties: HashMap::new(),
        }];
        let fc = FeatureCollection::new(features, None);

        let results = check_quality(&fc, &[QualityRule::GeometryType("Polygon".into())]);
        assert!(!results[0].passed);
    }
}
