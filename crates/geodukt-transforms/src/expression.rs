//! Expression engine — computed property columns.

use std::collections::HashMap;

use geo::{Area, Geometry, Length};
use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::pipeline::{PipelineError, TransformOp};

/// Expression operation: adds computed columns based on geometry or property expressions.
pub struct ExpressionTransform;

impl TransformOp for ExpressionTransform {
    fn apply(
        &self,
        input: &FeatureCollection,
        params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        // Parse expressions: {"output_col": "expression", ...}
        let expressions: HashMap<String, String> = params
            .get("expressions")
            .and_then(|v| v.as_table())
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let features: Vec<Feature> = input
            .features
            .iter()
            .map(|f| {
                let mut props = f.properties.clone();
                for (col, expr) in &expressions {
                    let value = evaluate_expression(expr, &f.geometry, &f.properties);
                    props.insert(col.clone(), value);
                }
                Feature {
                    geometry: f.geometry.clone(),
                    properties: props,
                }
            })
            .collect();

        Ok(FeatureCollection::new(features, input.crs.clone()))
    }
}

/// Simple expression evaluator supporting built-in functions.
fn evaluate_expression(
    expr: &str,
    geometry: &Geometry<f64>,
    properties: &HashMap<String, Value>,
) -> Value {
    let expr = expr.trim();

    // Built-in geometry functions
    match expr {
        "$area" => {
            let area = match geometry {
                Geometry::Polygon(p) => p.unsigned_area(),
                Geometry::MultiPolygon(mp) => mp.unsigned_area(),
                _ => 0.0,
            };
            Value::Float(area)
        }
        "$length" => {
            let length = match geometry {
                Geometry::LineString(ls) => {
                    geo::algorithm::line_measures::metric_spaces::Euclidean.length(ls)
                }
                Geometry::MultiLineString(mls) => mls
                    .0
                    .iter()
                    .map(|ls| geo::algorithm::line_measures::metric_spaces::Euclidean.length(ls))
                    .sum(),
                _ => 0.0,
            };
            Value::Float(length)
        }
        "$num_vertices" => {
            let count = count_vertices(geometry);
            Value::Integer(count)
        }
        "$geom_type" => Value::String(geometry_type_name(geometry).to_string()),
        _ => {
            // Property reference: $prop.field_name
            if let Some(field) = expr.strip_prefix("$prop.") {
                properties.get(field).cloned().unwrap_or(Value::Null)
            }
            // Arithmetic: field * number or field / number
            else if let Some((field, op, num)) = parse_arithmetic(expr) {
                match properties.get(&field) {
                    Some(Value::Float(v)) => Value::Float(apply_op(*v, op, num)),
                    Some(Value::Integer(v)) => Value::Float(apply_op(*v as f64, op, num)),
                    _ => Value::Null,
                }
            } else {
                // Literal string
                Value::String(expr.to_string())
            }
        }
    }
}

fn count_vertices(geom: &Geometry<f64>) -> i64 {
    match geom {
        Geometry::Point(_) => 1,
        Geometry::LineString(ls) => ls.0.len() as i64,
        Geometry::Polygon(p) => {
            p.exterior().0.len() as i64
                + p.interiors().iter().map(|r| r.0.len() as i64).sum::<i64>()
        }
        Geometry::MultiPoint(mp) => mp.0.len() as i64,
        Geometry::MultiLineString(mls) => mls.0.iter().map(|ls| ls.0.len() as i64).sum(),
        Geometry::MultiPolygon(mp) => {
            mp.0.iter()
                .map(|p| {
                    p.exterior().0.len() as i64
                        + p.interiors().iter().map(|r| r.0.len() as i64).sum::<i64>()
                })
                .sum()
        }
        _ => 0,
    }
}

fn geometry_type_name(geom: &Geometry<f64>) -> &'static str {
    match geom {
        Geometry::Point(_) => "Point",
        Geometry::LineString(_) => "LineString",
        Geometry::Polygon(_) => "Polygon",
        Geometry::MultiPoint(_) => "MultiPoint",
        Geometry::MultiLineString(_) => "MultiLineString",
        Geometry::MultiPolygon(_) => "MultiPolygon",
        _ => "Unknown",
    }
}

fn parse_arithmetic(expr: &str) -> Option<(String, char, f64)> {
    for op in ['*', '/', '+', '-'] {
        if let Some(pos) = expr.rfind(op) {
            let field = expr[..pos].trim().to_string();
            let num: f64 = expr[pos + 1..].trim().parse().ok()?;
            return Some((field, op, num));
        }
    }
    None
}

fn apply_op(value: f64, op: char, operand: f64) -> f64 {
    match op {
        '*' => value * operand,
        '/' => {
            if operand != 0.0 {
                value / operand
            } else {
                0.0
            }
        }
        '+' => value + operand,
        '-' => value - operand,
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::polygon;

    #[test]
    fn test_expression_area() {
        let poly = polygon![
            (x: 0.0, y: 0.0),
            (x: 2.0, y: 0.0),
            (x: 2.0, y: 3.0),
            (x: 0.0, y: 3.0),
            (x: 0.0, y: 0.0),
        ];
        let features = vec![Feature {
            geometry: Geometry::Polygon(poly),
            properties: HashMap::new(),
        }];
        let fc = FeatureCollection::new(features, None);

        let mut expr_table = toml::value::Table::new();
        expr_table.insert("area".into(), toml::Value::String("$area".into()));
        expr_table.insert("type".into(), toml::Value::String("$geom_type".into()));

        let params = HashMap::from([("expressions".into(), toml::Value::Table(expr_table))]);
        let result = ExpressionTransform.apply(&fc, &params).unwrap();

        assert_eq!(
            result.features[0].properties.get("area"),
            Some(&Value::Float(6.0))
        );
        assert_eq!(
            result.features[0].properties.get("type"),
            Some(&Value::String("Polygon".into()))
        );
    }

    #[test]
    fn test_expression_arithmetic() {
        let features = vec![Feature {
            geometry: Geometry::Point(geo::point!(x: 0.0, y: 0.0)),
            properties: HashMap::from([("population".into(), Value::Integer(1000))]),
        }];
        let fc = FeatureCollection::new(features, None);

        let mut expr_table = toml::value::Table::new();
        expr_table.insert(
            "pop_k".into(),
            toml::Value::String("population / 1000".into()),
        );

        let params = HashMap::from([("expressions".into(), toml::Value::Table(expr_table))]);
        let result = ExpressionTransform.apply(&fc, &params).unwrap();

        assert_eq!(
            result.features[0].properties.get("pop_k"),
            Some(&Value::Float(1.0))
        );
    }
}
