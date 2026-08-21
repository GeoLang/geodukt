//! Expression engine — computed property columns.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::geometry::FeatureGeometry;
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
        let expressions: HashMap<String, String> =
            crate::params::table(params, "expression", "expressions")?
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect();

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

    fn preserves_feature_order(&self) -> bool {
        true
    }
}

/// Simple expression evaluator supporting built-in functions.
fn evaluate_expression(
    expr: &str,
    geometry: &FeatureGeometry,
    properties: &HashMap<String, Value>,
) -> Value {
    let expr = expr.trim();

    // Built-in geometry functions
    match expr {
        "$area" => {
            let area = match geometry {
                FeatureGeometry::Polygon(p) => p.area(),
                FeatureGeometry::MultiPolygon(mp) => mp.area(),
                _ => 0.0,
            };
            Value::Float(area)
        }
        "$length" => {
            let length = match geometry {
                FeatureGeometry::LineString(ls) => ls.length(),
                FeatureGeometry::MultiLineString(mls) => mls.length(),
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

fn count_vertices(geom: &FeatureGeometry) -> i64 {
    match geom {
        FeatureGeometry::Point(_) => 1,
        FeatureGeometry::LineString(ls) => ls.coords().len() as i64,
        FeatureGeometry::Polygon(p) => polygon_vertices(p),
        FeatureGeometry::MultiPoint(mp) => mp.points().len() as i64,
        FeatureGeometry::MultiLineString(mls) => mls
            .linestrings()
            .iter()
            .map(|ls| ls.coords().len() as i64)
            .sum(),
        FeatureGeometry::MultiPolygon(mp) => mp.polygons().iter().map(polygon_vertices).sum(),
        FeatureGeometry::GeometryCollection(_) => 0,
    }
}

fn polygon_vertices(poly: &geodukt_core::geometry::Polygon) -> i64 {
    poly.exterior().coords().len() as i64
        + poly
            .interiors()
            .iter()
            .map(|r| r.coords().len() as i64)
            .sum::<i64>()
}

fn geometry_type_name(geom: &FeatureGeometry) -> &'static str {
    match geom {
        FeatureGeometry::GeometryCollection(_) => "Unknown",
        other => geodukt_core::geometry::type_name(other),
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
    use geodukt_core::geometry::{Coord, Point, Polygon, Ring};

    #[test]
    fn test_expression_area() {
        let poly = Polygon::new(
            Ring::new(vec![
                Coord::new(0.0, 0.0),
                Coord::new(2.0, 0.0),
                Coord::new(2.0, 3.0),
                Coord::new(0.0, 3.0),
                Coord::new(0.0, 0.0),
            ]),
            vec![],
        );
        let features = vec![Feature {
            geometry: FeatureGeometry::Polygon(poly),
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
            geometry: FeatureGeometry::Point(Point::new(0.0, 0.0)),
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
