//! Integration tests for geodukt-io formats.

use std::collections::HashMap;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_io::geopackage_io::{read_geopackage, write_geopackage};

#[test]
fn test_geopackage_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.gpkg");

    let mut props = HashMap::new();
    props.insert("name".into(), Value::String("hello".into()));
    props.insert("count".into(), Value::String("42".into()));

    let features = vec![
        Feature {
            geometry: geo::Geometry::Point(geo::Point::new(1.0, 2.0)),
            properties: props.clone(),
        },
        Feature {
            geometry: geo::Geometry::Point(geo::Point::new(3.0, 4.0)),
            properties: props.clone(),
        },
    ];

    let fc = FeatureCollection::new(features, None);
    write_geopackage(&path, &fc, "test_layer").unwrap();

    let result = read_geopackage(&path, Some("test_layer")).unwrap();
    assert_eq!(result.features.len(), 2);
    assert_eq!(
        result.features[0].properties.get("name"),
        Some(&Value::String("hello".into()))
    );
    assert_eq!(
        result.features[1].properties.get("count"),
        Some(&Value::String("42".into()))
    );
}

#[test]
fn test_geopackage_auto_table_detection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auto.gpkg");

    let mut props = HashMap::new();
    props.insert("id".into(), Value::String("1".into()));

    let fc = FeatureCollection::new(
        vec![Feature {
            geometry: geo::Geometry::Point(geo::Point::new(0.0, 0.0)),
            properties: props,
        }],
        None,
    );
    write_geopackage(&path, &fc, "auto_layer").unwrap();

    // Read without specifying table name
    let result = read_geopackage(&path, None).unwrap();
    assert_eq!(result.features.len(), 1);
}

#[test]
fn test_geopackage_preserves_geometry_and_crs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("geometry.gpkg");

    let polygon = geo::Geometry::Polygon(geo::Polygon::new(
        geo::LineString::from(vec![
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 4.0),
            (0.0, 4.0),
            (0.0, 0.0),
        ]),
        vec![geo::LineString::from(vec![
            (1.0, 1.0),
            (2.0, 1.0),
            (2.0, 2.0),
            (1.0, 2.0),
            (1.0, 1.0),
        ])],
    ));
    let line = geo::Geometry::LineString(geo::LineString::from(vec![
        (0.0, 0.0),
        (1.0, 2.0),
        (3.0, 4.0),
    ]));

    let fc = FeatureCollection::new(
        vec![
            Feature {
                geometry: polygon.clone(),
                properties: HashMap::new(),
            },
            Feature {
                geometry: line.clone(),
                properties: HashMap::new(),
            },
        ],
        Some("EPSG:3857".into()),
    );
    write_geopackage(&path, &fc, "shapes").unwrap();

    let back = read_geopackage(&path, Some("shapes")).unwrap();
    assert_eq!(back.crs.as_deref(), Some("EPSG:3857"));
    assert_eq!(back.features[0].geometry, polygon);
    assert_eq!(back.features[1].geometry, line);
}

#[test]
fn test_geopackage_preserves_attribute_types() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("types.gpkg");

    let fc = FeatureCollection::new(
        vec![
            Feature {
                geometry: geo::Geometry::Point(geo::Point::new(1.0, 1.0)),
                properties: HashMap::from([
                    ("label".into(), Value::String("first".into())),
                    ("count".into(), Value::Integer(7)),
                    ("ratio".into(), Value::Float(0.25)),
                ]),
            },
            // second feature omits a key, so the column must come back null
            Feature {
                geometry: geo::Geometry::Point(geo::Point::new(2.0, 2.0)),
                properties: HashMap::from([("count".into(), Value::Integer(9))]),
            },
        ],
        None,
    );
    write_geopackage(&path, &fc, "typed").unwrap();

    let back = read_geopackage(&path, Some("typed")).unwrap();
    assert_eq!(
        back.features[0].properties.get("count"),
        Some(&Value::Integer(7))
    );
    assert_eq!(
        back.features[0].properties.get("ratio"),
        Some(&Value::Float(0.25))
    );
    assert_eq!(back.features[1].properties.get("label"), Some(&Value::Null));
    // an absent CRS is written as the EPSG:4326 default the readers produce
    assert_eq!(back.crs.as_deref(), Some("EPSG:4326"));
}

#[test]
fn test_geopackage_declares_geometry_type() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("declared.gpkg");

    let fc = FeatureCollection::new(
        vec![Feature {
            geometry: geo::Geometry::Point(geo::Point::new(0.0, 0.0)),
            properties: HashMap::new(),
        }],
        None,
    );
    write_geopackage(&path, &fc, "pts").unwrap();

    let conn = rusqlite::Connection::open(&path).unwrap();
    let declared: String = conn
        .query_row(
            "SELECT geometry_type_name FROM gpkg_geometry_columns WHERE table_name='pts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(declared, "POINT");
}

#[test]
fn test_geopackage_write_replaces_layer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rerun.gpkg");

    let fc = FeatureCollection::new(
        vec![Feature {
            geometry: geo::Geometry::Point(geo::Point::new(1.0, 1.0)),
            properties: HashMap::from([("id".into(), Value::Integer(1))]),
        }],
        None,
    );
    write_geopackage(&path, &fc, "layer_a").unwrap();
    write_geopackage(&path, &fc, "layer_b").unwrap();
    write_geopackage(&path, &fc, "layer_a").unwrap();

    // re-running the same sink replaces its rows and leaves the other layer alone
    assert_eq!(read_geopackage(&path, Some("layer_a")).unwrap().len(), 1);
    assert_eq!(read_geopackage(&path, Some("layer_b")).unwrap().len(), 1);
}
