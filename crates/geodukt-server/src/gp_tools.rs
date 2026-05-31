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
use geodukt_transforms::simplify::SimplifyTransform;

/// Catalog entry for a geoprocessing tool.
#[derive(Debug, Clone, Serialize)]
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: &'static [ParamDef],
}

/// Parameter definition for tool catalog.
#[derive(Debug, Clone, Serialize)]
pub struct ParamDef {
    pub name: &'static str,
    pub param_type: &'static str,
    pub required: bool,
    pub description: &'static str,
}

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

/// List all available GP tools.
async fn catalog() -> Json<Vec<ToolInfo>> {
    Json(vec![
        ToolInfo {
            name: "buffer",
            description: "Buffer geometries by a distance",
            parameters: &[ParamDef {
                name: "distance",
                param_type: "f64",
                required: true,
                description: "Buffer distance in CRS units",
            }],
        },
        ToolInfo {
            name: "centroid",
            description: "Compute centroids of input geometries",
            parameters: &[],
        },
        ToolInfo {
            name: "clip",
            description: "Clip input features to a bounding box",
            parameters: &[
                ParamDef {
                    name: "min_x",
                    param_type: "f64",
                    required: false,
                    description: "Minimum X coordinate (default -180)",
                },
                ParamDef {
                    name: "min_y",
                    param_type: "f64",
                    required: false,
                    description: "Minimum Y coordinate (default -90)",
                },
                ParamDef {
                    name: "max_x",
                    param_type: "f64",
                    required: false,
                    description: "Maximum X coordinate (default 180)",
                },
                ParamDef {
                    name: "max_y",
                    param_type: "f64",
                    required: false,
                    description: "Maximum Y coordinate (default 90)",
                },
            ],
        },
        ToolInfo {
            name: "dissolve",
            description: "Dissolve features by a grouping attribute",
            parameters: &[ParamDef {
                name: "field",
                param_type: "string",
                required: true,
                description: "Attribute field to dissolve by",
            }],
        },
        ToolInfo {
            name: "simplify",
            description: "Simplify geometries using Douglas-Peucker algorithm",
            parameters: &[ParamDef {
                name: "tolerance",
                param_type: "f64",
                required: true,
                description: "Simplification tolerance in CRS units",
            }],
        },
    ])
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

    let min_x = req
        .params
        .get("min_x")
        .and_then(|v| v.as_f64())
        .unwrap_or(-180.0);
    let min_y = req
        .params
        .get("min_y")
        .and_then(|v| v.as_f64())
        .unwrap_or(-90.0);
    let max_x = req
        .params
        .get("max_x")
        .and_then(|v| v.as_f64())
        .unwrap_or(180.0);
    let max_y = req
        .params
        .get("max_y")
        .and_then(|v| v.as_f64())
        .unwrap_or(90.0);

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
    let field = req
        .params
        .get("field")
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_REQUEST, "Missing 'field' parameter".into()))?;

    let params = HashMap::from([("field".to_string(), toml::Value::String(field.to_string()))]);
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
    let tolerance = req
        .params
        .get("tolerance")
        .and_then(|v| v.as_f64())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "Missing 'tolerance' parameter".into(),
        ))?;

    let params = HashMap::from([("tolerance".to_string(), toml::Value::Float(tolerance))]);
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

        let body = serde_json::json!({"input": input, "params": {"tolerance": 0.2}});

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
        assert!(gp_resp.feature_count >= 1);
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
                }
            ]
        });

        let body = serde_json::json!({"input": input, "params": {"field": "group"}});

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
        // Two features with same group should dissolve to one
        assert_eq!(gp_resp.feature_count, 1);
    }
}
