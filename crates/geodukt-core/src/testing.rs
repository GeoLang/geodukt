//! Testing framework — assert output schemas, counts, geometry types, spatial relationships.

use crate::feature::FeatureCollection;
use crate::geometry::FeatureGeometry;
use crate::quality::{QualityRule, check_quality};

/// A test assertion for pipeline output validation.
#[derive(Debug, Clone)]
pub struct PipelineTest {
    pub name: String,
    pub node: String,
    pub assertions: Vec<TestAssertion>,
}

/// Individual test assertion.
#[derive(Debug, Clone)]
pub enum TestAssertion {
    /// Output must have exactly N features.
    FeatureCount(usize),
    /// Output must have at least N features.
    MinFeatures(usize),
    /// Output must have at most N features.
    MaxFeatures(usize),
    /// All features must have this property.
    HasProperty(String),
    /// All geometries must be of this type.
    GeometryType(String),
    /// Custom quality rule.
    QualityRule(QualityRule),
    /// CRS must match.
    Crs(String),
    /// No empty geometries.
    NoEmpty,
}

/// Result of running a pipeline test.
#[derive(Debug)]
pub struct TestResult {
    pub test_name: String,
    pub passed: bool,
    pub details: Vec<AssertionResult>,
}

/// Result of a single assertion.
#[derive(Debug)]
pub struct AssertionResult {
    pub assertion: String,
    pub passed: bool,
    pub message: String,
}

/// Run pipeline tests against actual output.
pub fn run_tests(
    tests: &[PipelineTest],
    outputs: &std::collections::HashMap<String, FeatureCollection>,
) -> Vec<TestResult> {
    tests
        .iter()
        .map(|test| {
            let details = match outputs.get(&test.node) {
                Some(fc) => test
                    .assertions
                    .iter()
                    .map(|a| check_assertion(a, fc))
                    .collect(),
                None => vec![AssertionResult {
                    assertion: "node_exists".into(),
                    passed: false,
                    message: format!("Node '{}' not found in outputs", test.node),
                }],
            };
            let passed = details.iter().all(|d| d.passed);
            TestResult {
                test_name: test.name.clone(),
                passed,
                details,
            }
        })
        .collect()
}

fn check_assertion(assertion: &TestAssertion, fc: &FeatureCollection) -> AssertionResult {
    match assertion {
        TestAssertion::FeatureCount(expected) => AssertionResult {
            assertion: format!("feature_count == {expected}"),
            passed: fc.len() == *expected,
            message: format!("actual: {}", fc.len()),
        },
        TestAssertion::MinFeatures(min) => AssertionResult {
            assertion: format!("feature_count >= {min}"),
            passed: fc.len() >= *min,
            message: format!("actual: {}", fc.len()),
        },
        TestAssertion::MaxFeatures(max) => AssertionResult {
            assertion: format!("feature_count <= {max}"),
            passed: fc.len() <= *max,
            message: format!("actual: {}", fc.len()),
        },
        TestAssertion::HasProperty(prop) => {
            let all_have = fc.features.iter().all(|f| f.properties.contains_key(prop));
            AssertionResult {
                assertion: format!("has_property({prop})"),
                passed: all_have,
                message: if all_have {
                    "all features have property".into()
                } else {
                    "some features missing property".into()
                },
            }
        }
        TestAssertion::GeometryType(expected) => {
            let results = check_quality(fc, &[QualityRule::GeometryType(expected.clone())]);
            let passed = results.first().is_some_and(|r| r.passed);
            AssertionResult {
                assertion: format!("geometry_type({expected})"),
                passed,
                message: results
                    .first()
                    .map(|r| r.message.clone())
                    .unwrap_or_default(),
            }
        }
        TestAssertion::QualityRule(rule) => {
            let results = check_quality(fc, std::slice::from_ref(rule));
            let r = &results[0];
            AssertionResult {
                assertion: r.rule.clone(),
                passed: r.passed,
                message: r.message.clone(),
            }
        }
        TestAssertion::Crs(expected) => AssertionResult {
            assertion: format!("crs == {expected}"),
            passed: fc.crs.as_deref() == Some(expected.as_str()),
            message: format!("actual: {:?}", fc.crs),
        },
        TestAssertion::NoEmpty => {
            let empty_count = fc
                .features
                .iter()
                .filter(|f| is_empty_geometry(&f.geometry))
                .count();
            AssertionResult {
                assertion: "no_empty_geometries".into(),
                passed: empty_count == 0,
                message: if empty_count == 0 {
                    "no empty geometries".into()
                } else {
                    format!("{empty_count} empty geometries found")
                },
            }
        }
    }
}

fn is_empty_geometry(geom: &FeatureGeometry) -> bool {
    match geom {
        FeatureGeometry::LineString(ls) => ls.coords().is_empty(),
        FeatureGeometry::Polygon(p) => p.exterior().coords().is_empty(),
        FeatureGeometry::MultiPoint(mp) => mp.points().is_empty(),
        FeatureGeometry::MultiLineString(mls) => mls.linestrings().is_empty(),
        FeatureGeometry::MultiPolygon(mp) => mp.polygons().is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{Feature, Value};
    use crate::geometry::Point;
    use std::collections::HashMap;

    #[test]
    fn test_pipeline_tests() {
        let features = vec![Feature {
            geometry: FeatureGeometry::Point(Point::new(1.0, 2.0)),
            properties: HashMap::from([("name".into(), Value::String("test".into()))]),
        }];
        let fc = FeatureCollection::new(features, Some("EPSG:4326".into()));
        let mut outputs = std::collections::HashMap::new();
        outputs.insert("output".into(), fc);

        let tests = vec![PipelineTest {
            name: "check_output".into(),
            node: "output".into(),
            assertions: vec![
                TestAssertion::FeatureCount(1),
                TestAssertion::HasProperty("name".into()),
                TestAssertion::GeometryType("Point".into()),
                TestAssertion::Crs("EPSG:4326".into()),
                TestAssertion::NoEmpty,
            ],
        }];

        let results = run_tests(&tests, &outputs);
        assert!(results[0].passed);
        assert!(results[0].details.iter().all(|d| d.passed));
    }
}
