//! A collection wide enough to tile many times over, filtered on the engine
//! and filtered directly, compared feature by feature. The registry handed to
//! `execute` is empty, so the run only succeeds on the engine path.

use std::collections::HashMap;
use std::sync::Mutex;

use geodukt_core::feature::{Feature, FeatureCollection, Properties, Value};
use geodukt_core::geometry::{Coord, FeatureGeometry, LineString, Polygon, Ring};
use geodukt_core::manifest::{Manifest, Sink, Source};
use geodukt_core::pipeline::{Pipeline, PipelineError, SinkWriter, SourceReader, TransformOp};
use geodukt_transforms::filter::FilterTransform;

/// vertices one unit apart along axis-aligned edges, so the collection's
/// median segment length, and with it the engine's base resolution, is one
fn along(corners: &[(f64, f64)]) -> Vec<Coord> {
    let mut out = Vec::new();
    for pair in corners.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let steps = ((b.0 - a.0).abs() + (b.1 - a.1).abs()).round() as usize;
        let step = |d: f64| if d == 0.0 { 0.0 } else { d.signum() };
        let (dx, dy) = (step(b.0 - a.0), step(b.1 - a.1));
        for s in 0..steps {
            out.push(Coord::new(a.0 + dx * s as f64, a.1 + dy * s as f64));
        }
    }
    let last = *corners.last().expect("corners");
    out.push(Coord::new(last.0, last.1));
    out
}

fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Ring {
    Ring::new(along(&[
        (min_x, min_y),
        (max_x, min_y),
        (max_x, max_y),
        (min_x, max_y),
        (min_x, min_y),
    ]))
}

fn feature(geometry: FeatureGeometry, kind: &str, n: i64) -> Feature {
    Feature {
        geometry,
        properties: Properties::from([
            ("kind".into(), Value::String(kind.into())),
            ("n".into(), Value::Integer(n)),
        ]),
    }
}

/// the zone spans nine tiles, the road eight, the patch sits inside one, and
/// the extent as a whole is forty tiles across
fn collection() -> FeatureCollection {
    FeatureCollection::new(
        vec![
            feature(
                FeatureGeometry::Polygon(Polygon::new(rect(0.0, 0.0, 700.0, 700.0), vec![])),
                "keep",
                0,
            ),
            feature(
                FeatureGeometry::LineString(LineString::new(along(&[
                    (0.0, 1200.0),
                    (1800.0, 1200.0),
                ]))),
                "keep",
                1,
            ),
            feature(
                FeatureGeometry::Polygon(Polygon::new(rect(1500.0, 50.0, 1520.0, 70.0), vec![])),
                "keep",
                2,
            ),
            feature(
                FeatureGeometry::Polygon(Polygon::new(rect(900.0, 900.0, 920.0, 920.0), vec![])),
                "skip",
                3,
            ),
        ],
        Some("EPSG:4326".into()),
    )
}

struct Reader;

impl SourceReader for Reader {
    fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
        Ok(collection())
    }
}

#[derive(Default)]
struct Writer(Mutex<Option<FeatureCollection>>);

impl SinkWriter for Writer {
    fn write_sink(&self, data: &FeatureCollection, _sink: &Sink) -> Result<(), PipelineError> {
        *self.0.lock().unwrap() = Some(data.clone());
        Ok(())
    }
}

const MANIFEST: &str = r#"
[project]
name = "parity"

[[source]]
name = "input"
format = "geojson"
path = "in.geojson"

[[transform]]
name = "kept"
input = "input"
operation = "filter"
field = "kind"
equals = "keep"

[[sink]]
name = "out"
input = "kept"
format = "geojson"
path = "out.geojson"
"#;

fn area(geometry: &FeatureGeometry) -> f64 {
    match geometry {
        FeatureGeometry::Polygon(p) => p.area(),
        FeatureGeometry::MultiPolygon(mp) => mp.area(),
        _ => 0.0,
    }
}

fn ends(geometry: &FeatureGeometry) -> Option<(Coord, Coord)> {
    let FeatureGeometry::LineString(line) = geometry else {
        return None;
    };
    Some((*line.coords().first()?, *line.coords().last()?))
}

#[test]
fn test_a_tiled_engine_filter_matches_the_direct_filter() {
    let pipeline = Pipeline::new(Manifest::from_toml(MANIFEST).unwrap()).unwrap();
    let writer = Writer::default();
    let report = pipeline
        .execute(&Reader, &HashMap::new(), &writer)
        .expect("the engine runs the filter without a registered operation");
    let counts: Vec<usize> = report.steps.iter().map(|s| s.feature_count).collect();
    assert_eq!(counts, vec![4, 3, 3], "counts are features, not fragments");

    let params = HashMap::from([
        ("field".into(), toml::Value::String("kind".into())),
        ("equals".into(), toml::Value::String("keep".into())),
    ]);
    let direct = FilterTransform.apply(&collection(), &params).unwrap();
    let through_engine = writer.0.lock().unwrap().take().unwrap();

    assert_eq!(through_engine.len(), direct.len());
    assert_eq!(through_engine.crs, direct.crs);
    for (got, want) in through_engine.features.iter().zip(&direct.features) {
        assert_eq!(got.properties, want.properties);
        let (a, b) = (area(&got.geometry), area(&want.geometry));
        assert!(
            (a - b).abs() <= 1e-9 * b.abs().max(1.0),
            "feature {:?} came back with area {a}, want {b}",
            want.properties.get("n")
        );
        assert_eq!(ends(&got.geometry), ends(&want.geometry));
    }

    // the pieces the tiles cut the zone and the road into merged back into
    // one geometry each, not a bag of fragments
    assert!(
        matches!(
            through_engine.features[0].geometry,
            FeatureGeometry::Polygon(_)
        ),
        "the zone came back as {:?}",
        through_engine.features[0].geometry
    );
    assert!((area(&through_engine.features[0].geometry) - 490_000.0).abs() < 1e-6);
    assert!(matches!(
        through_engine.features[1].geometry,
        FeatureGeometry::LineString(_)
    ));
}
