//! Collections wide enough to tile many times over, put through an operation
//! on the engine and through the same operation directly, compared feature by
//! feature. The operation handed to `execute` refuses to run, so a green run
//! is one the engine carried.

use std::collections::HashMap;
use std::sync::Mutex;

use geodukt_core::feature::{Feature, FeatureCollection, Properties, Value};
use geodukt_core::geometry::{self, Coord, FeatureGeometry, LineString, Point, Polygon, Ring};
use geodukt_core::manifest::{Manifest, Sink, Source};
use geodukt_core::pipeline::{Pipeline, PipelineError, SinkWriter, SourceReader, TransformOp};
use geodukt_transforms::clip::ClipTransform;
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

/// lines on the tile seams the half-open membership rule decides. the extent
/// is exactly two tiles each way, so the interior seams sit at x = 256 and
/// y = 256 and the widened window's edges fall on tile boundaries too
fn seam_collection() -> FeatureCollection {
    let line = |a: (f64, f64), b: (f64, f64), n: i64| {
        feature(
            FeatureGeometry::LineString(LineString::new(along(&[a, b]))),
            "keep",
            n,
        )
    };
    FeatureCollection::new(
        vec![
            line((256.0, 0.0), (256.0, 512.0), 0),
            line((0.0, 256.0), (512.0, 256.0), 1),
            line((512.0, 0.0), (512.0, 512.0), 2),
            line((0.0, 0.0), (512.0, 0.0), 3),
        ],
        Some("EPSG:4326".into()),
    )
}

struct Reader(FeatureCollection);

impl SourceReader for Reader {
    fn read_source(&self, _source: &Source) -> Result<FeatureCollection, PipelineError> {
        Ok(self.0.clone())
    }
}

/// registered so the manifest names something real, and loud when it runs,
/// which only happens when the routing did not send the filter to the engine
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

fn length(geometry: &FeatureGeometry) -> f64 {
    let lines: Vec<&LineString> = match geometry {
        FeatureGeometry::LineString(l) => vec![l],
        FeatureGeometry::MultiLineString(mls) => mls.linestrings().iter().collect(),
        _ => Vec::new(),
    };
    lines
        .iter()
        .flat_map(|l| l.coords().windows(2).map(|w| w[0].distance_to(&w[1])))
        .sum()
}

/// what the two paths made of one feature, compared the way `FeatureGeometry`
/// allows: it carries no `PartialEq`
fn same_geometry(got: &FeatureGeometry, want: &FeatureGeometry, what: &str) {
    assert_eq!(
        geometry::type_name(got),
        geometry::type_name(want),
        "{what} came back as {got:?}"
    );
    assert_eq!(
        geometry::coords(got),
        geometry::coords(want),
        "{what} lost or gained vertices"
    );
}

/// run the manifest through `execute` with `op` registered but refusing to
/// run, so only the engine can produce a result
fn through_engine(
    manifest: &str,
    op: &str,
    fc: &FeatureCollection,
) -> (FeatureCollection, Vec<usize>) {
    let transforms: HashMap<String, Box<dyn TransformOp>> =
        HashMap::from([(op.to_string(), Box::new(OffEngine) as Box<dyn TransformOp>)]);
    let pipeline = Pipeline::new(Manifest::from_toml(manifest).unwrap()).unwrap();
    let writer = Writer::default();
    let report = pipeline
        .execute(&Reader(fc.clone()), &transforms, &writer)
        .expect("the engine runs the operation, the registered one refuses to");
    let counts = report.steps.iter().map(|s| s.feature_count).collect();
    let out = writer.0.lock().unwrap().take().unwrap();
    (out, counts)
}

/// the same collection filtered both ways, on the engine and directly
fn both_paths(fc: FeatureCollection) -> (FeatureCollection, FeatureCollection, Vec<usize>) {
    let (through_engine, counts) = through_engine(MANIFEST, "filter", &fc);

    let params = HashMap::from([
        ("field".into(), toml::Value::String("kind".into())),
        ("equals".into(), toml::Value::String("keep".into())),
    ]);
    let direct = FilterTransform.apply(&fc, &params).unwrap();
    (through_engine, direct, counts)
}

#[test]
fn test_a_tiled_engine_filter_matches_the_direct_filter() {
    let (through_engine, direct, counts) = both_paths(collection());
    assert_eq!(counts, vec![4, 3, 3], "counts are features, not fragments");

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

/// a line lying along a tile seam belongs to one side of it, so it comes back
/// once rather than as two copies stitched into a multilinestring
#[test]
fn test_lines_on_the_tile_seams_come_back_whole() {
    let (through_engine, direct, counts) = both_paths(seam_collection());
    assert_eq!(counts, vec![4, 4, 4]);
    assert_eq!(through_engine.len(), 4);

    for (got, want) in through_engine.features.iter().zip(&direct.features) {
        let n = format!("feature {:?}", want.properties.get("n"));
        same_geometry(&got.geometry, &want.geometry, &n);
        assert_eq!(length(&got.geometry), length(&want.geometry), "{n}");
    }
}

const CLIP_MANIFEST: &str = r#"
[project]
name = "clip-parity"

[[source]]
name = "input"
format = "geojson"
path = "in.geojson"

[[transform]]
name = "clipped"
input = "input"
operation = "clip"
min_x = 100.0
min_y = 100.0
max_x = 400.0
max_y = 400.0

[[sink]]
name = "out"
input = "clipped"
format = "geojson"
path = "out.geojson"
"#;

/// lines crossing the clip boundary and the tile seams at once, plus a point
/// inside the box and one outside it
fn clip_collection() -> FeatureCollection {
    FeatureCollection::new(
        vec![
            feature(
                FeatureGeometry::LineString(LineString::new(along(&[
                    (0.0, 200.0),
                    (512.0, 200.0),
                ]))),
                "keep",
                0,
            ),
            feature(
                FeatureGeometry::LineString(LineString::new(along(&[
                    (300.0, 0.0),
                    (300.0, 512.0),
                ]))),
                "keep",
                1,
            ),
            feature(FeatureGeometry::Point(Point::new(200.0, 200.0)), "keep", 2),
            feature(FeatureGeometry::Point(Point::new(450.0, 450.0)), "keep", 3),
        ],
        Some("EPSG:4326".into()),
    )
}

/// `ClipTransform` and the engine's clip are the same code now, so the two
/// paths agree on what survives and on its geometry
#[test]
fn test_engine_clip_matches_the_direct_clip() {
    let fc = clip_collection();
    let (through_engine, counts) = through_engine(CLIP_MANIFEST, "clip", &fc);

    let params = HashMap::from([
        ("min_x".into(), toml::Value::Float(100.0)),
        ("min_y".into(), toml::Value::Float(100.0)),
        ("max_x".into(), toml::Value::Float(400.0)),
        ("max_y".into(), toml::Value::Float(400.0)),
    ]);
    let direct = ClipTransform::new().apply(&fc, &params).unwrap();

    assert_eq!(direct.len(), 3, "the point outside the box is dropped");
    assert_eq!(counts, vec![4, 3, 3], "and the engine drops it too");
    assert_eq!(through_engine.len(), direct.len());
    assert_eq!(through_engine.crs, direct.crs);
    for (got, want) in through_engine.features.iter().zip(&direct.features) {
        let n = format!("feature {:?}", want.properties.get("n"));
        assert_eq!(got.properties, want.properties, "{n}");
        same_geometry(&got.geometry, &want.geometry, &n);
    }

    // the lines really were cut, not carried through whole
    assert_eq!(
        ends(&through_engine.features[0].geometry),
        Some((Coord::new(100.0, 200.0), Coord::new(400.0, 200.0)))
    );
    assert_eq!(
        ends(&through_engine.features[1].geometry),
        Some((Coord::new(300.0, 100.0), Coord::new(300.0, 400.0)))
    );
}
