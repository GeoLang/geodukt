//! The engine path end to end. The mappable operations are registered, since
//! routing an operation the caller never registered would hide a broken
//! manifest, but their implementations refuse to run: a green run is a run
//! geoplumb carried.

use std::collections::HashMap;
use std::sync::Mutex;

use geodukt_core::feature::{Feature, FeatureCollection, Properties, Value};
use geodukt_core::geometry::{Coord, FeatureGeometry, Polygon, Ring};
use geodukt_core::manifest::{Manifest, Sink, Source};
use geodukt_core::pipeline::{Pipeline, PipelineError, SinkWriter, SourceReader, TransformOp};

fn square(x: f64, kind: &str) -> Feature {
    Feature {
        geometry: FeatureGeometry::Polygon(Polygon::new(
            Ring::new(vec![
                Coord::new(x, 0.0),
                Coord::new(x + 1.0, 0.0),
                Coord::new(x + 1.0, 1.0),
                Coord::new(x, 1.0),
                Coord::new(x, 0.0),
            ]),
            vec![],
        )),
        properties: Properties::from([
            ("kind".into(), Value::String(kind.into())),
            ("n".into(), Value::Integer(x as i64)),
        ]),
    }
}

struct Reader;

impl SourceReader for Reader {
    fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
        Ok(FeatureCollection::new(
            vec![
                square(0.0, "road"),
                square(2.0, "park"),
                square(4.0, "road"),
                square(6.0, "park"),
            ],
            Some("EPSG:4326".into()),
        ))
    }
}

#[derive(Default)]
struct Writer(Mutex<HashMap<String, FeatureCollection>>);

impl SinkWriter for Writer {
    fn write_sink(&self, data: &FeatureCollection, sink: &Sink) -> Result<(), PipelineError> {
        self.0
            .lock()
            .unwrap()
            .insert(sink.name.clone(), data.clone());
        Ok(())
    }
}

/// an operation the engine cannot run, so the chain under it stays off it
struct Passthrough;

impl TransformOp for Passthrough {
    fn apply(
        &self,
        input: &FeatureCollection,
        _params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        Ok(input.clone())
    }
}

/// registered so the manifest names something real, and loud when it is
/// reached, which only happens when the routing sent the work the long way
struct OffEngine;

impl TransformOp for OffEngine {
    fn apply(
        &self,
        _input: &FeatureCollection,
        _params: &HashMap<String, toml::Value>,
    ) -> Result<FeatureCollection, PipelineError> {
        Err(PipelineError::Transform {
            name: "off_engine".into(),
            message: "ran off the engine".into(),
        })
    }
}

fn registry(ops: Vec<(&str, Box<dyn TransformOp>)>) -> HashMap<String, Box<dyn TransformOp>> {
    ops.into_iter()
        .map(|(name, op)| (name.to_string(), op))
        .collect()
}

const HEAD: &str = r#"
[project]
name = "engine"

[[source]]
name = "input"
format = "geojson"
path = "in.geojson"
"#;

fn run(
    toml: &str,
    transforms: HashMap<String, Box<dyn TransformOp>>,
) -> (Vec<(String, usize)>, Writer) {
    let pipeline = Pipeline::new(Manifest::from_toml(toml).unwrap()).unwrap();
    let writer = Writer::default();
    let report = pipeline.execute(&Reader, &transforms, &writer).unwrap();
    let steps = report
        .steps
        .iter()
        .map(|s| (s.name.clone(), s.feature_count))
        .collect();
    (steps, writer)
}

#[test]
fn test_a_mappable_head_runs_on_the_engine() {
    let toml = format!(
        r#"{HEAD}
[[transform]]
name = "roads"
input = "input"
operation = "filter"
field = "kind"
equals = "road"

[[transform]]
name = "tagged"
input = "roads"
operation = "schema_map"
drop = ["kind"]
add = {{ source = "engine" }}

[[sink]]
name = "out"
input = "tagged"
format = "geojson"
path = "out.geojson"
"#
    );

    let (steps, writer) = run(
        &toml,
        registry(vec![
            ("filter", Box::new(OffEngine)),
            ("schema_map", Box::new(OffEngine)),
        ]),
    );
    assert_eq!(
        steps,
        vec![
            ("input".to_string(), 4),
            ("roads".to_string(), 2),
            ("tagged".to_string(), 2),
            ("out".to_string(), 2),
        ]
    );

    let written = writer.0.lock().unwrap();
    let out = written.get("out").unwrap();
    assert_eq!(out.crs, Some("EPSG:4326".into()));
    assert_eq!(out.len(), 2);
    let ns: Vec<Option<&Value>> = out.features.iter().map(|f| f.properties.get("n")).collect();
    assert_eq!(
        ns,
        vec![Some(&Value::Integer(0)), Some(&Value::Integer(4))],
        "the matching features keep their source order"
    );
    for f in &out.features {
        assert!(!f.properties.contains_key("kind"), "dropped");
        assert_eq!(
            f.properties.get("source"),
            Some(&Value::String("engine".into()))
        );
    }
    // a feature inside a single tile makes the round trip untouched
    let FeatureGeometry::Polygon(p) = &out.features[0].geometry else {
        panic!("a polygon in, a polygon out");
    };
    assert_eq!(p.exterior().coords()[0], Coord::new(0.0, 0.0));
    assert_eq!(p.exterior().coords().len(), 5);
}

/// an operation the engine cannot run cuts the chain, and everything under it
/// needs a registered operation again
#[test]
fn test_an_unmappable_head_keeps_the_filter_under_it_on_the_registry() {
    let toml = format!(
        r#"{HEAD}
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

[[sink]]
name = "out"
input = "roads"
format = "geojson"
path = "out.geojson"
"#
    );

    let transforms = registry(vec![
        ("reproject", Box::new(Passthrough)),
        ("filter", Box::new(OffEngine)),
    ]);

    let pipeline = Pipeline::new(Manifest::from_toml(&toml).unwrap()).unwrap();
    let failure = pipeline
        .execute(&Reader, &transforms, &Writer::default())
        .unwrap_err();
    assert_eq!(failure.failed.as_ref().unwrap().name, "roads");
    assert!(
        failure.error().to_string().contains("ran off the engine"),
        "{}",
        failure.error()
    );
    assert_eq!(failure.completed.len(), 2);
    assert_eq!(failure.not_run, vec!["out"]);
}

/// the engine runs an operation on the caller's behalf, never in place of an
/// operation the caller never registered
#[test]
fn test_an_unregistered_operation_still_fails_the_run() {
    let toml = format!(
        r#"{HEAD}
[[transform]]
name = "roads"
input = "input"
operation = "filter"
field = "kind"
equals = "road"

[[sink]]
name = "out"
input = "roads"
format = "geojson"
path = "out.geojson"
"#
    );

    let pipeline = Pipeline::new(Manifest::from_toml(&toml).unwrap()).unwrap();
    let failure = pipeline
        .execute(&Reader, &HashMap::new(), &Writer::default())
        .unwrap_err();
    assert_eq!(failure.failed.as_ref().unwrap().name, "roads");
    assert!(
        failure.error().to_string().contains("unknown operation"),
        "{}",
        failure.error()
    );
}

/// a source with nothing mappable under it never makes the round trip, so the
/// sink sees the collection the reader handed over
#[test]
fn test_a_format_conversion_bypasses_the_engine() {
    let toml = format!(
        r#"{HEAD}
[[sink]]
name = "out"
input = "input"
format = "geojson"
path = "out.geojson"
"#
    );

    let (steps, writer) = run(&toml, HashMap::new());
    assert_eq!(
        steps,
        vec![("input".to_string(), 4), ("out".to_string(), 4)]
    );
    let written = writer.0.lock().unwrap();
    let out = written.get("out").unwrap();
    assert_eq!(
        out.features[1].properties.get("kind"),
        Some(&Value::String("park".into()))
    );
}

/// a collapsed ring has bbox so the engine will hold it, and zero area so
/// GeometryValid rejects it. a filter under the source is resident.
fn collapsed_road() -> Feature {
    Feature {
        geometry: FeatureGeometry::Polygon(Polygon::new(
            Ring::new(vec![
                Coord::new(0.0, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(1.0, 1.0),
                Coord::new(1.0, 0.0),
                Coord::new(0.0, 0.0),
            ]),
            vec![],
        )),
        properties: Properties::from([("kind".into(), Value::String("road".into()))]),
    }
}

struct InvalidReader;

impl SourceReader for InvalidReader {
    fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
        Ok(FeatureCollection::new(
            vec![collapsed_road(), square(2.0, "road")],
            Some("EPSG:4326".into()),
        ))
    }
}

#[test]
fn test_quality_fails_a_resident_filter_with_an_invalid_polygon() {
    let toml = r#"
[project]
name = "engine"
quality = true

[[source]]
name = "input"
format = "geojson"
path = "in.geojson"

[[transform]]
name = "roads"
input = "input"
operation = "filter"
field = "kind"
equals = "road"

[[sink]]
name = "out"
input = "roads"
format = "geojson"
path = "out.geojson"
"#;

    let pipeline = Pipeline::new(Manifest::from_toml(toml).unwrap()).unwrap();
    let failure = pipeline
        .execute(
            &InvalidReader,
            &registry(vec![("filter", Box::new(OffEngine))]),
            &Writer::default(),
        )
        .unwrap_err();
    let failed = failure.failed.as_ref().unwrap();
    assert_eq!(failed.name, "roads");
    assert!(
        failed.message.contains("invalid geometr"),
        "{}",
        failed.message
    );
    assert!(
        !failed.message.contains("ran off the engine"),
        "{}",
        failed.message
    );
}

static CWD: Mutex<()> = Mutex::new(());

#[test]
fn test_lineage_file_is_non_empty_for_an_engine_only_wave() {
    let toml = r#"
[project]
name = "engine"
lineage = true

[[source]]
name = "input"
format = "geojson"
path = "in.geojson"

[[transform]]
name = "roads"
input = "input"
operation = "filter"
field = "kind"
equals = "road"

[[sink]]
name = "out"
input = "roads"
format = "geojson"
path = "out.geojson"
"#;

    let _cwd = CWD.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let orig = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let result = Pipeline::new(Manifest::from_toml(toml).unwrap())
        .unwrap()
        .execute(
            &Reader,
            &registry(vec![("filter", Box::new(OffEngine))]),
            &Writer::default(),
        );
    let json = std::fs::read_to_string(".geodukt/lineage.json");
    let _ = std::env::set_current_dir(orig);
    result.unwrap();
    let json = json.unwrap();
    let lineage: geodukt_core::lineage::LineageTracker = serde_json::from_str(&json).unwrap();
    assert!(
        !lineage.records.is_empty(),
        "engine-only lineage was {json}"
    );
    assert!(
        lineage.records.iter().any(|r| r.output_node == "roads"),
        "{json}"
    );
}
