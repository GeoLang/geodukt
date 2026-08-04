//! Routing the engine-mappable head of a pipeline onto geoplumb.
//!
//! A source whose data the engine can hold, and whose children start with an
//! operation the engine can run, becomes a `VecSrc`. The run of mappable
//! transforms under it becomes elements. Everything else stays on the
//! [`crate::pipeline::TransformOp`] path, reached through a full-extent pull
//! whose fragments are dissolved back into whole features.

use std::collections::{HashMap, HashSet};

use geoplumb::elements::{VecClip, VecFilter, VecSchema, VecSrc};
use geoplumb::{Bbox, Crs, Engine, Graph, NodeId, Transform as Element, VectorChunk, WindowReq};

use crate::dag::Node;
use crate::feature::{Feature, FeatureCollection, Properties, Value};
use crate::geometry::{self, Coord, MultiPolygon, Polygon, Ring};
use crate::manifest::{Source, Transform};
use crate::pipeline::PipelineError;

/// chunk cache one run may hold in memory
const CACHE_BUDGET: usize = 256 << 20;

/// One engine per engine-resident source. A geodukt transform has exactly one
/// input, so the resident nodes under a source form a tree of their own, and
/// building the engine when the run reads that source keeps every read at its
/// place in the execution order.
struct EngineRun {
    engine: Engine,
    nodes: HashMap<String, NodeId>,
    /// the source extent, which none of the mappable operations grows
    extent: Bbox,
    crs: Option<String>,
}

/// Which nodes of a run go on the engine, and the engines holding them.
pub struct EngineRouter {
    /// transforms in topological order: name, input, and whether the operation
    /// and its parameters map to an element
    transforms: Vec<(String, String, bool)>,
    engines: Vec<EngineRun>,
    owner: HashMap<String, usize>,
}

impl EngineRouter {
    pub fn new(order: &[&Node]) -> EngineRouter {
        let transforms = order
            .iter()
            .filter_map(|node| match node {
                Node::Transform(t) => Some((t.name.clone(), t.input.clone(), element(t).is_some())),
                _ => None,
            })
            .collect();
        EngineRouter {
            transforms,
            engines: Vec::new(),
            owner: HashMap::new(),
        }
    }

    pub fn is_resident(&self, name: &str) -> bool {
        self.owner.contains_key(name)
    }

    /// Put a source and the mappable transforms under it on the engine, and
    /// say whether it went on. A source feeding no mappable transform stays
    /// off, so a pure format conversion never makes the round trip.
    pub fn admit_source(
        &mut self,
        order: &[&Node],
        source: &Source,
        fc: &FeatureCollection,
    ) -> Result<bool, PipelineError> {
        let mappable_child = self
            .transforms
            .iter()
            .any(|(_, input, mappable)| *mappable && input == &source.name);
        if !mappable_child {
            return Ok(false);
        }
        let (Some(code), Some(bounds), Some(collection)) =
            (crs_code(&fc.crs), extent(fc), engine_collection(fc))
        else {
            return Ok(false);
        };

        let mut graph = Graph::new();
        let src = VecSrc::new(collection, Crs(code)).map_err(|e| PipelineError::Source {
            name: source.name.clone(),
            message: format!("engine source: {e}"),
        })?;
        let root = graph.add_source(Box::new(src));
        let mut nodes = HashMap::from([(source.name.clone(), root)]);
        for node in order {
            let Node::Transform(t) = node else { continue };
            let (Some(&parent), Some(el)) = (nodes.get(&t.input), element(t)) else {
                continue;
            };
            nodes.insert(t.name.clone(), graph.add_transform(parent, el));
        }
        let engine = Engine::new(graph, CACHE_BUDGET).map_err(|e| PipelineError::Source {
            name: source.name.clone(),
            message: format!("engine graph: {e}"),
        })?;

        // tile membership is half open, x in [min, max) and y in (min, max],
        // so the window needs a pixel of room past max_x and min_y or the
        // features sitting there vanish. the other two edges are the grid
        // origin: widening them would pull the tiles across the seam the data
        // starts on, and geometry lying along a seam belongs to both sides
        let margin = engine.grid(root).base_resolution;
        let extent = Bbox::new(bounds.0, bounds.1 - margin, bounds.2 + margin, bounds.3);
        let index = self.engines.len();
        for name in nodes.keys() {
            self.owner.insert(name.clone(), index);
        }
        self.engines.push(EngineRun {
            engine,
            nodes,
            extent,
            crs: fc.crs.clone(),
        });
        Ok(true)
    }

    /// True when something downstream of `name` runs off the engine, so its
    /// features have to come back.
    pub fn feeds_off_engine(&self, order: &[&Node], name: &str) -> bool {
        order.iter().any(|node| match node {
            Node::Transform(t) => t.input == name && !self.is_resident(&t.name),
            Node::Sink(s) => s.input == name,
            Node::Source(_) => false,
        })
    }

    /// Full-extent pull of a resident node: how many features it holds, and
    /// the features themselves when `materialize` is set.
    pub fn pull(
        &self,
        name: &str,
        materialize: bool,
    ) -> Result<(usize, Option<FeatureCollection>), PipelineError> {
        let run = &self.engines[self.owner[name]];
        let node = run.nodes[name];
        let req = WindowReq {
            bbox: run.extent,
            resolution: run.engine.grid(node).base_resolution,
        };
        let chunk = futures::executor::block_on(run.engine.pull(node, req))
            .and_then(|chunk| chunk.into_vector())
            .map_err(|e| PipelineError::Transform {
                name: name.to_string(),
                message: format!("engine pull: {e}"),
            })?;
        // every fragment of a feature carries the feature's id, so the
        // distinct ids are the feature count
        let count = chunk
            .features
            .iter()
            .map(|f| f.id)
            .collect::<HashSet<u64>>()
            .len();
        let collection = materialize
            .then(|| collection_from(&chunk.dissolve(), name, &run.crs))
            .transpose()?;
        Ok((count, collection))
    }
}

/// The element an operation maps to, when the engine can run it with these
/// parameters. Anything else keeps the transform, and everything under it, on
/// the [`crate::pipeline::TransformOp`] path.
fn element(t: &Transform) -> Option<Box<dyn Element>> {
    match t.operation.as_str() {
        "filter" => Some(Box::new(VecFilter::new(
            t.params.get("field")?.as_str()?,
            scalar(t.params.get("equals")?)?,
        ))),
        "schema_map" => Some(Box::new(schema(t)?)),
        "clip" => Some(Box::new(clip(t)?)),
        _ => None,
    }
}

fn schema(t: &Transform) -> Option<VecSchema> {
    if !["rename", "drop", "add"]
        .iter()
        .any(|key| t.params.contains_key(*key))
    {
        return None;
    }
    let rename = t
        .params
        .get("rename")
        .and_then(|v| v.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let drop = t
        .params
        .get("drop")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let add = match t.params.get("add").and_then(|v| v.as_table()) {
        Some(table) => table
            .iter()
            .map(|(k, v)| Some((k.clone(), scalar(v)?)))
            .collect::<Option<HashMap<String, serde_json::Value>>>()?,
        None => HashMap::new(),
    };
    Some(VecSchema { rename, drop, add })
}

fn clip(t: &Transform) -> Option<VecClip> {
    let edge = |name: &str| -> Option<f64> { number(t.params.get(name)?) };
    let (min_x, min_y) = (edge("min_x")?, edge("min_y")?);
    let (max_x, max_y) = (edge("max_x")?, edge("max_y")?);
    let ring = Ring::new(vec![
        Coord::new(min_x, min_y),
        Coord::new(max_x, min_y),
        Coord::new(max_x, max_y),
        Coord::new(min_x, max_y),
        Coord::new(min_x, min_y),
    ]);
    Some(VecClip {
        boundary: MultiPolygon::new(vec![Polygon::new(ring, vec![])]),
    })
}

/// A manifest may write a number as `0` or `0.0` and mean the same edge.
fn number(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// A parameter the engine can compare or store. Datetimes, arrays and tables
/// have no scalar json form, so they keep the operation off the engine.
fn scalar(v: &toml::Value) -> Option<serde_json::Value> {
    match v {
        toml::Value::String(s) => Some(s.clone().into()),
        toml::Value::Integer(i) => Some((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map(Into::into),
        toml::Value::Boolean(b) => Some((*b).into()),
        _ => None,
    }
}

/// A property value as json. A non-finite float has no json number, so its
/// collection stays off the engine.
pub fn to_json(value: &Value) -> Option<serde_json::Value> {
    Some(match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::Integer(i) => (*i).into(),
        Value::Float(f) => serde_json::Number::from_f64(*f)?.into(),
        Value::String(s) => s.clone().into(),
    })
}

/// A property value back from json. Only scalars go on the engine, so an
/// array or object means something rewrote the properties behind our back.
pub fn from_json(value: &serde_json::Value) -> Option<Value> {
    Some(match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Float(n.as_f64()?),
        },
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => return None,
    })
}

/// The EPSG code a collection carries. An absent crs is wgs84, and anything
/// that is not `EPSG:<code>` keeps the collection off the engine.
fn crs_code(crs: &Option<String>) -> Option<u32> {
    let Some(crs) = crs else { return Some(4326) };
    let (authority, code) = crs.split_once(':')?;
    if !authority.eq_ignore_ascii_case("EPSG") {
        return None;
    }
    code.parse().ok()
}

/// Bounds of the whole collection. None when it is empty, when a feature
/// carries no coordinates, or when a coordinate is not finite: a pull would
/// lose the feature or land it nowhere.
fn extent(fc: &FeatureCollection) -> Option<(f64, f64, f64, f64)> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for f in &fc.features {
        let coords = geometry::coords(&f.geometry);
        if coords.is_empty() {
            return None;
        }
        for c in coords {
            if !c.x.is_finite() || !c.y.is_finite() {
                return None;
            }
            bounds = Some(match bounds {
                None => (c.x, c.y, c.x, c.y),
                Some(b) => (b.0.min(c.x), b.1.min(c.y), b.2.max(c.x), b.3.max(c.y)),
            });
        }
    }
    bounds
}

/// The collection as topoi features, or None when a property has no json
/// form. Features keep their order, which is the order `VecSrc` numbers them.
fn engine_collection(fc: &FeatureCollection) -> Option<topoi_core::geojson::FeatureCollection> {
    let features = fc
        .features
        .iter()
        .map(|f| {
            let properties = f
                .properties
                .iter()
                .map(|(k, v)| Some((k.clone(), to_json(v)?)))
                .collect::<Option<HashMap<String, serde_json::Value>>>()?;
            Some(topoi_core::geojson::Feature {
                geometry: Some(f.geometry.clone()),
                properties,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(topoi_core::geojson::FeatureCollection { features })
}

/// Dissolved fragments as a geodukt collection. `dissolve` orders by feature
/// id and `VecSrc` numbers in collection order, so this is the source order.
fn collection_from(
    chunk: &VectorChunk,
    name: &str,
    crs: &Option<String>,
) -> Result<FeatureCollection, PipelineError> {
    let mut features = Vec::with_capacity(chunk.features.len());
    for f in &chunk.features {
        let mut properties = Properties::with_capacity(f.properties.len());
        for (k, v) in &f.properties {
            let value = from_json(v).ok_or_else(|| PipelineError::Transform {
                name: name.to_string(),
                message: format!("property '{k}' came back from the engine as {v}"),
            })?;
            properties.insert(k.clone(), value);
        }
        features.push(Feature {
            geometry: f.geometry.clone(),
            properties,
        });
    }
    Ok(FeatureCollection::new(features, crs.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::Dag;
    use crate::geometry::{FeatureGeometry, LineString, Point};
    use crate::manifest::Manifest;

    fn point(x: f64, y: f64, kind: &str) -> Feature {
        Feature {
            geometry: FeatureGeometry::Point(Point::new(x, y)),
            properties: Properties::from([
                ("kind".into(), Value::String(kind.into())),
                ("n".into(), Value::Integer(x as i64)),
            ]),
        }
    }

    /// two points a unit apart, so the source grid has a base resolution
    fn collection() -> FeatureCollection {
        FeatureCollection::new(
            vec![
                point(0.0, 0.0, "road"),
                point(1.0, 0.0, "building"),
                point(2.0, 0.0, "road"),
            ],
            Some("EPSG:4326".into()),
        )
    }

    fn manifest(body: &str) -> Manifest {
        Manifest::from_toml(&format!(
            r#"
[project]
name = "routing"

[[source]]
name = "input"
format = "geojson"
path = "in.geojson"
{body}
"#
        ))
        .unwrap()
    }

    const FILTER: &str = r#"
[[transform]]
name = "roads"
input = "input"
operation = "filter"
field = "kind"
equals = "road"
"#;

    const SINK: &str = r#"
[[sink]]
name = "out"
input = "roads"
format = "geojson"
path = "out.geojson"
"#;

    /// residency over a manifest, with the source data the run would read
    fn residents(body: &str, fc: &FeatureCollection) -> Vec<String> {
        let manifest = manifest(body);
        let dag = Dag::from_manifest(&manifest).unwrap();
        let order = dag.topological_order().unwrap();
        let mut router = EngineRouter::new(&order);
        for node in &order {
            if let Node::Source(s) = node {
                router.admit_source(&order, s, fc).unwrap();
            }
        }
        let mut names: Vec<String> = order
            .iter()
            .map(|n| n.name().to_string())
            .filter(|n| router.is_resident(n))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn test_filter_into_a_sink_is_resident() {
        assert_eq!(
            residents(&format!("{FILTER}{SINK}"), &collection()),
            vec!["input", "roads"]
        );
    }

    #[test]
    fn test_a_reproject_head_keeps_the_filter_under_it_off_the_engine() {
        let body = format!(
            r#"
[[transform]]
name = "projected"
input = "input"
operation = "reproject"
to_crs = "EPSG:3857"

[[transform]]
name = "roads"
input = "projected"
operation = "filter"
field = "kind"
equals = "road"
{SINK}
"#
        );
        assert!(residents(&body, &collection()).is_empty());
    }

    #[test]
    fn test_a_source_feeding_only_a_sink_bypasses_the_engine() {
        let body = r#"
[[sink]]
name = "out"
input = "input"
format = "geojson"
path = "out.geojson"
"#;
        assert!(residents(body, &collection()).is_empty());
    }

    #[test]
    fn test_an_empty_source_bypasses_the_engine() {
        let body = format!("{FILTER}{SINK}");
        assert!(residents(&body, &FeatureCollection::empty()).is_empty());
    }

    #[test]
    fn test_a_datetime_filter_parameter_bypasses_the_engine() {
        let body = format!(
            r#"
[[transform]]
name = "roads"
input = "input"
operation = "filter"
field = "kind"
equals = 1979-05-27T07:32:00Z
{SINK}
"#
        );
        assert!(residents(&body, &collection()).is_empty());
    }

    #[test]
    fn test_a_non_finite_property_bypasses_the_engine() {
        let mut fc = collection();
        fc.features[0]
            .properties
            .insert("bad".into(), Value::Float(f64::NAN));
        assert!(residents(&format!("{FILTER}{SINK}"), &fc).is_empty());
    }

    #[test]
    fn test_an_unparsable_crs_bypasses_the_engine() {
        let mut fc = collection();
        fc.crs = Some("urn:ogc:def:crs:OGC:1.3:CRS84".into());
        assert!(residents(&format!("{FILTER}{SINK}"), &fc).is_empty());
    }

    /// the chain runs on the engine as far as the first operation that does
    /// not map, and no further
    #[test]
    fn test_residency_stops_at_the_first_unmappable_operation() {
        let body = format!(
            r#"
{FILTER}
[[transform]]
name = "projected"
input = "roads"
operation = "reproject"
to_crs = "EPSG:3857"

[[transform]]
name = "named"
input = "projected"
operation = "filter"
field = "kind"
equals = "road"

[[sink]]
name = "out"
input = "named"
format = "geojson"
path = "out.geojson"
"#
        );
        assert_eq!(residents(&body, &collection()), vec!["input", "roads"]);
    }

    #[test]
    fn test_property_values_survive_the_json_round_trip() {
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::Integer(-7),
            Value::Float(2.5),
            Value::String("road".into()),
        ] {
            let json = to_json(&value).unwrap();
            assert_eq!(from_json(&json).unwrap(), value, "{value:?}");
        }
        assert!(to_json(&Value::Float(f64::NAN)).is_none());
        assert!(from_json(&serde_json::json!([1, 2])).is_none());
    }

    /// the boundary round trip: collection in, pull and dissolve out, with
    /// order, properties and crs intact
    #[test]
    fn test_a_pull_gives_back_the_features_it_was_given() {
        let manifest = manifest(&format!("{FILTER}{SINK}"));
        let dag = Dag::from_manifest(&manifest).unwrap();
        let order = dag.topological_order().unwrap();
        let mut router = EngineRouter::new(&order);
        let Node::Source(source) = order[0] else {
            panic!("the source comes first");
        };
        let fc = collection();
        assert!(router.admit_source(&order, source, &fc).unwrap());

        let (count, materialized) = router.pull("input", true).unwrap();
        assert_eq!(count, 3);
        let back = materialized.unwrap();
        assert_eq!(back.crs, fc.crs);
        let kinds: Vec<&Value> = back
            .features
            .iter()
            .map(|f| f.properties.get("kind").unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                &Value::String("road".into()),
                &Value::String("building".into()),
                &Value::String("road".into()),
            ],
            "features keep their source order"
        );
        assert_eq!(
            back.features[1].properties.get("n"),
            Some(&Value::Integer(1))
        );
        for (got, want) in back.features.iter().zip(&fc.features) {
            assert!(geometry::equals(&got.geometry, &want.geometry));
        }
    }

    #[test]
    fn test_a_filter_pull_counts_and_keeps_only_the_matches() {
        let manifest = manifest(&format!("{FILTER}{SINK}"));
        let dag = Dag::from_manifest(&manifest).unwrap();
        let order = dag.topological_order().unwrap();
        let mut router = EngineRouter::new(&order);
        let Node::Source(source) = order[0] else {
            panic!("the source comes first");
        };
        router.admit_source(&order, source, &collection()).unwrap();

        assert!(router.feeds_off_engine(&order, "roads"), "a sink reads it");
        let (count, materialized) = router.pull("roads", true).unwrap();
        assert_eq!(count, 2);
        let back = materialized.unwrap();
        assert_eq!(back.len(), 2);
        for f in &back.features {
            assert_eq!(
                f.properties.get("kind"),
                Some(&Value::String("road".into()))
            );
        }
    }

    /// a line long enough to cross tile seams comes back whole
    #[test]
    fn test_a_seam_split_line_dissolves_back_to_one_feature() {
        let coords: Vec<Coord> = (0..2000).map(|i| Coord::new(i as f64, 0.0)).collect();
        let fc = FeatureCollection::new(
            vec![Feature {
                geometry: FeatureGeometry::LineString(LineString::new(coords.clone())),
                properties: Properties::from([("kind".into(), Value::String("road".into()))]),
            }],
            None,
        );
        let manifest = manifest(&format!("{FILTER}{SINK}"));
        let dag = Dag::from_manifest(&manifest).unwrap();
        let order = dag.topological_order().unwrap();
        let mut router = EngineRouter::new(&order);
        let Node::Source(source) = order[0] else {
            panic!("the source comes first");
        };
        router.admit_source(&order, source, &fc).unwrap();

        let (count, materialized) = router.pull("roads", true).unwrap();
        assert_eq!(count, 1);
        let back = materialized.unwrap();
        assert_eq!(back.len(), 1);
        let FeatureGeometry::LineString(line) = &back.features[0].geometry else {
            panic!("the line stitches back into one line");
        };
        assert_eq!(line.coords().first(), Some(&coords[0]));
        assert_eq!(line.coords().last(), coords.last());
    }
}
