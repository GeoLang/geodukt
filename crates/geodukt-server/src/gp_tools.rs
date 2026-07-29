//! Geoprocessing tools service — individual spatial operations exposed as REST endpoints.
//!
//! Modeled after ArcGIS Geoprocessing Services / Google Earth Engine compute endpoints.
//! Each tool takes a GeoJSON input + parameters and returns processed GeoJSON.

use std::collections::HashMap;

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::pipeline::TransformOp;
use geodukt_transforms::buffer::BufferTransform;
use geodukt_transforms::centroid::CentroidTransform;
use geodukt_transforms::clip::ClipTransform;
use geodukt_transforms::dissolve::DissolveTransform;
use geodukt_transforms::registry::{OperationSpec, operation};
use geodukt_transforms::simplify::SimplifyTransform;

/// The operations exposed as GP endpoints, a subset of the pipeline's operations.
/// Kept in the same order as the routes below.
const GP_TOOLS: &[&str] = &["buffer", "centroid", "clip", "dissolve", "simplify"];

/// GeoJSON-like input for GP tools.
#[derive(Debug, Deserialize)]
pub struct GpRequest {
    /// GeoJSON FeatureCollection as raw JSON.
    pub input: serde_json::Value,
    /// Tool-specific parameters.
    #[serde(default)]
    pub params: HashMap<String, serde_json::Value>,
}

/// GP tool response.
#[derive(Debug, Serialize, Deserialize)]
pub struct GpResponse {
    pub tool: String,
    pub feature_count: usize,
    pub output: serde_json::Value,
}

/// GP tool error.
type GpError = (StatusCode, String);

/// Create the GP tools router (mounted under /gp).
pub fn gp_routes() -> Router {
    Router::new()
        .route("/catalog", axum::routing::get(catalog))
        .route("/buffer", post(buffer_tool))
        .route("/centroid", post(centroid_tool))
        .route("/clip", post(clip_tool))
        .route("/dissolve", post(dissolve_tool))
        .route("/simplify", post(simplify_tool))
}

/// List all available GP tools, described by the same table the pipeline
/// dispatches on so the parameter names here are the ones the transforms read.
async fn catalog() -> Json<Vec<OperationSpec>> {
    Json(
        GP_TOOLS
            .iter()
            .map(|name| {
                operation(name).expect("every GP tool is an operation in the registry table")
            })
            .collect(),
    )
}

/// Parse a GeoJSON FeatureCollection into internal representation.
fn parse_input(input: &serde_json::Value) -> Result<FeatureCollection, GpError> {
    let geojson_str = serde_json::to_string(input)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid JSON: {e}")))?;

    let gj: geojson::GeoJson = geojson_str
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid GeoJSON: {e}")))?;

    let fc = match gj {
        geojson::GeoJson::FeatureCollection(fc) => fc,
        geojson::GeoJson::Feature(f) => geojson::FeatureCollection {
            bbox: None,
            features: vec![f],
            foreign_members: None,
        },
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Expected FeatureCollection or Feature".into(),
            ));
        }
    };

    let features: Vec<Feature> = fc
        .features
        .into_iter()
        .filter_map(|f| {
            let geom = f.geometry?.try_into().ok()?;
            let props = f
                .properties
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::Null => Value::Null,
                        serde_json::Value::Bool(b) => Value::Bool(b),
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                Value::Integer(i)
                            } else {
                                Value::Float(n.as_f64().unwrap_or(0.0))
                            }
                        }
                        serde_json::Value::String(s) => Value::String(s),
                        other => Value::String(other.to_string()),
                    };
                    (k, val)
                })
                .collect();
            Some(Feature {
                geometry: geom,
                properties: props,
            })
        })
        .collect();

    Ok(FeatureCollection::new(features, None))
}

/// Convert internal features back to GeoJSON Value.
fn features_to_geojson(fc: &FeatureCollection) -> serde_json::Value {
    let features: Vec<geojson::Feature> = fc
        .features
        .iter()
        .map(|f| {
            let geom: geojson::Geometry = (&f.geometry).into();
            let props: serde_json::Map<String, serde_json::Value> = f
                .properties
                .iter()
                .map(|(k, v)| {
                    let jv = match v {
                        Value::Null => serde_json::Value::Null,
                        Value::Bool(b) => serde_json::Value::Bool(*b),
                        Value::Integer(i) => serde_json::json!(i),
                        Value::Float(fl) => serde_json::json!(fl),
                        Value::String(s) => serde_json::Value::String(s.clone()),
                    };
                    (k.clone(), jv)
                })
                .collect();
            geojson::Feature {
                bbox: None,
                geometry: Some(geom),
                id: None,
                properties: Some(props),
                foreign_members: None,
            }
        })
        .collect();

    let fc_out = geojson::FeatureCollection {
        bbox: None,
        features,
        foreign_members: None,
    };
    serde_json::to_value(fc_out).unwrap_or_default()
}

async fn buffer_tool(Json(req): Json<GpRequest>) -> Result<Json<GpResponse>, GpError> {
    let input = parse_input(&req.input)?;
    let distance = req.params.get("distance").and_then(|v| v.as_f64()).ok_or((
        StatusCode::BAD_REQUEST,
        "Missing 'distance' parameter".into(),
    ))?;

    let params = HashMap::from([("distance".to_string(), toml::Value::Float(distance))]);
    let result = BufferTransform
        .apply(&input, &params)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GpResponse {
        tool: "buffer".into(),
        feature_count: result.len(),
        output: features_to_geojson(&result),
    }))
}

async fn centroid_tool(Json(req): Json<GpRequest>) -> Result<Json<GpResponse>, GpError> {
    let input = parse_input(&req.input)?;

    let result = CentroidTransform
        .apply(&input, &HashMap::new())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GpResponse {
        tool: "centroid".into(),
        feature_count: result.len(),
        output: features_to_geojson(&result),
    }))
}

async fn clip_tool(Json(req): Json<GpRequest>) -> Result<Json<GpResponse>, GpError> {
    let input = parse_input(&req.input)?;

    // no whole-world default: a clip missing an edge used to return the input
    // untouched, which reads as a working clip that did nothing
    let edge = |name: &str| {
        req.params.get(name).and_then(|v| v.as_f64()).ok_or((
            StatusCode::BAD_REQUEST,
            format!("Missing '{name}' parameter"),
        ))
    };
    let min_x = edge("min_x")?;
    let min_y = edge("min_y")?;
    let max_x = edge("max_x")?;
    let max_y = edge("max_y")?;

    let params = HashMap::from([
        ("min_x".to_string(), toml::Value::Float(min_x)),
        ("min_y".to_string(), toml::Value::Float(min_y)),
        ("max_x".to_string(), toml::Value::Float(max_x)),
        ("max_y".to_string(), toml::Value::Float(max_y)),
    ]);

    let result = ClipTransform::new()
        .apply(&input, &params)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GpResponse {
        tool: "clip".into(),
        feature_count: result.len(),
        output: features_to_geojson(&result),
    }))
}

async fn dissolve_tool(Json(req): Json<GpRequest>) -> Result<Json<GpResponse>, GpError> {
    let input = parse_input(&req.input)?;
    let group_by = req.params.get("group_by").and_then(|v| v.as_str()).ok_or((
        StatusCode::BAD_REQUEST,
        "Missing 'group_by' parameter".into(),
    ))?;

    let params = HashMap::from([(
        "group_by".to_string(),
        toml::Value::String(group_by.to_string()),
    )]);
    let result = DissolveTransform
        .apply(&input, &params)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GpResponse {
        tool: "dissolve".into(),
        feature_count: result.len(),
        output: features_to_geojson(&result),
    }))
}

async fn simplify_tool(Json(req): Json<GpRequest>) -> Result<Json<GpResponse>, GpError> {
    let input = parse_input(&req.input)?;
    let epsilon = req.params.get("epsilon").and_then(|v| v.as_f64()).ok_or((
        StatusCode::BAD_REQUEST,
        "Missing 'epsilon' parameter".into(),
    ))?;

    let params = HashMap::from([("epsilon".to_string(), toml::Value::Float(epsilon))]);
    let result = SimplifyTransform
        .apply(&input, &params)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GpResponse {
        tool: "simplify".into(),
        feature_count: result.len(),
        output: features_to_geojson(&result),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_router() -> Router {
        gp_routes()
    }

    #[tokio::test]
    async fn test_catalog() {
        let app = test_router();
        let req = Request::builder()
            .uri("/catalog")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let tools: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tools.len(), 5);
        assert_eq!(tools[0]["name"], "buffer");
    }

    #[tokio::test]
    async fn test_buffer_tool() {
        let app = test_router();
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
                "properties": {}
            }]
        });

        let body = serde_json::json!({
            "input": input,
            "params": {"distance": 1.0}
        });

        let req = Request::builder()
            .method("POST")
            .uri("/buffer")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let gp_resp: GpResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(gp_resp.tool, "buffer");
        assert_eq!(gp_resp.feature_count, 1);
    }

    #[tokio::test]
    async fn test_centroid_tool() {
        let app = test_router();
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]]]
                },
                "properties": {"name": "square"}
            }]
        });

        let body = serde_json::json!({"input": input, "params": {}});

        let req = Request::builder()
            .method("POST")
            .uri("/centroid")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let gp_resp: GpResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(gp_resp.tool, "centroid");
        assert_eq!(gp_resp.feature_count, 1);
    }

    #[tokio::test]
    async fn test_simplify_tool() {
        let app = test_router();
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[0.0, 0.0], [0.5, 0.1], [1.0, 0.0], [1.5, 0.1], [2.0, 0.0]]
                },
                "properties": {}
            }]
        });

        let body = serde_json::json!({"input": input, "params": {"epsilon": 0.2}});

        let req = Request::builder()
            .method("POST")
            .uri("/simplify")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let gp_resp: GpResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(gp_resp.tool, "simplify");
        assert_eq!(gp_resp.feature_count, 1);

        // epsilon has to reach the transform, so the zigzag loses its middle points
        let coords = gp_resp.output["features"][0]["geometry"]["coordinates"]
            .as_array()
            .unwrap()
            .len();
        assert!(coords < 5, "expected simplification, got {coords} points");
    }

    #[tokio::test]
    async fn test_missing_param_returns_400() {
        let app = test_router();
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
                "properties": {}
            }]
        });

        let body = serde_json::json!({"input": input, "params": {}});

        let req = Request::builder()
            .method("POST")
            .uri("/buffer")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_dissolve_tool() {
        let app = test_router();
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]]]
                    },
                    "properties": {"group": "a"}
                },
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 0.0]]]
                    },
                    "properties": {"group": "a"}
                },
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[5.0, 5.0], [6.0, 5.0], [6.0, 6.0], [5.0, 6.0], [5.0, 5.0]]]
                    },
                    "properties": {"group": "b"}
                }
            ]
        });

        let body = serde_json::json!({"input": input, "params": {"group_by": "group"}});

        let req = Request::builder()
            .method("POST")
            .uri("/dissolve")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let gp_resp: GpResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(gp_resp.tool, "dissolve");
        // group_by has to reach the transform: the two group 'a' polygons union
        // and group 'b' stays on its own. ignoring the parameter would give 1.
        assert_eq!(gp_resp.feature_count, 2);
    }

    /// The catalog describes the parameters the transforms actually read, because
    /// both come from the registry table. This used to drift: the catalog
    /// advertised 'field' and 'tolerance' while the transforms read 'group_by'
    /// and 'epsilon', so those parameters were silently ignored.
    #[tokio::test]
    async fn test_catalog_parameters_match_the_registry_table() {
        let app = test_router();
        let req = Request::builder()
            .uri("/catalog")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let tools: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();

        assert_eq!(tools.len(), GP_TOOLS.len());
        for (tool, name) in tools.iter().zip(GP_TOOLS) {
            assert_eq!(tool["name"], *name);
            let expected: Vec<&str> = operation(name)
                .unwrap()
                .parameters
                .iter()
                .map(|p| p.name)
                .collect();
            let listed: Vec<&str> = tool["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["name"].as_str().unwrap())
                .collect();
            assert_eq!(listed, expected, "{name} parameters");
        }
    }

    #[tokio::test]
    async fn test_dissolve_without_group_by_returns_400() {
        let app = test_router();
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
                "properties": {}
            }]
        });
        let body = serde_json::json!({"input": input, "params": {"field": "group"}});

        let req = Request::builder()
            .method("POST")
            .uri("/dissolve")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_clip_rejects_a_missing_edge() {
        // a clip that silently kept the whole world read as a clip that worked
        let input = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [0.0, 0.0]},
                "properties": {}
            }]
        });
        let body = serde_json::json!({
            "input": input,
            "params": {"min_x": -1.0, "min_y": -1.0, "max_x": 1.0}
        });
        let req = Request::builder()
            .method("POST")
            .uri("/clip")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let resp = test_router().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("max_y"));
    }
}
