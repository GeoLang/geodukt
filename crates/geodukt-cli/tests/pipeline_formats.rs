//! End-to-end pipeline runs over the formats a manifest can name, wired the
//! same way `geodukt run` wires them.

use std::collections::HashMap;
use std::path::Path;

use geodukt_core::feature::{Feature, FeatureCollection, Value};
use geodukt_core::geometry::{Coord, FeatureGeometry, Point, Polygon, equals};
use geodukt_core::manifest::Manifest;
use geodukt_core::pipeline::Pipeline;
use geodukt_io::formats::{MultiFormatReader, MultiFormatWriter};
use geodukt_io::geopackage_io::{read_geopackage, write_geopackage};
use geodukt_io::shapefile_io::read_shapefile;
use geodukt_transforms::registry::default_registry;

fn run(manifest_toml: &str) -> Vec<usize> {
    let manifest = Manifest::from_toml(manifest_toml).unwrap();
    let pipeline = Pipeline::new(manifest).unwrap();
    let report = pipeline
        .execute(&MultiFormatReader, &default_registry(), &MultiFormatWriter)
        .unwrap();
    report.steps.iter().map(|s| s.feature_count).collect()
}

fn square(offset: f64) -> FeatureGeometry {
    FeatureGeometry::Polygon(Polygon::from_coords(&[
        Coord::new(offset, offset),
        Coord::new(offset + 2.0, offset),
        Coord::new(offset + 2.0, offset + 2.0),
        Coord::new(offset, offset + 2.0),
    ]))
}

fn parcel(offset: f64, name: &str, id: i64, area: f64) -> Feature {
    Feature {
        geometry: square(offset),
        properties: HashMap::from([
            ("name".to_string(), Value::String(name.to_string())),
            ("id".to_string(), Value::Integer(id)),
            ("area".to_string(), Value::Float(area)),
        ]),
    }
}

#[test]
fn test_geopackage_through_transform_to_geopackage() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("parcels.gpkg");
    let output = dir.path().join("out").join("centroids.gpkg");

    let source = FeatureCollection::new(
        vec![parcel(0.0, "north", 1, 4.0), parcel(10.0, "south", 2, 4.5)],
        Some("EPSG:4326".into()),
    );
    write_geopackage(&input, &source, "parcels").unwrap();

    let counts = run(&format!(
        r#"
[project]
name = "gpkg-to-gpkg"

[[source]]
name = "parcels"
format = "geopackage"
path = "{input}"
layer = "parcels"

[[transform]]
name = "centers"
input = "parcels"
operation = "centroid"

[[sink]]
name = "out"
input = "centers"
format = "geopackage"
path = "{output}"
layer = "centroids"
"#,
        input = input.display(),
        output = output.display()
    ));
    assert_eq!(counts, vec![2, 2, 2]);

    let written = read_geopackage(&output, Some("centroids")).unwrap();
    assert_eq!(written.len(), 2);
    assert_eq!(written.crs.as_deref(), Some("EPSG:4326"));

    // geometry became a centroid point, attributes kept their types
    let by_name = |name: &str| {
        written
            .features
            .iter()
            .find(|f| f.properties.get("name") == Some(&Value::String(name.into())))
            .unwrap()
            .clone()
    };
    let north = by_name("north");
    assert!(equals(
        &north.geometry,
        &FeatureGeometry::Point(Point::new(1.0, 1.0))
    ));
    assert_eq!(north.properties.get("id"), Some(&Value::Integer(1)));
    assert_eq!(north.properties.get("area"), Some(&Value::Float(4.0)));

    let south = by_name("south");
    assert!(equals(
        &south.geometry,
        &FeatureGeometry::Point(Point::new(11.0, 11.0))
    ));
    assert_eq!(south.properties.get("area"), Some(&Value::Float(4.5)));
}

#[test]
fn test_geojson_to_shapefile() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("zones.geojson");
    let output = dir.path().join("out").join("zones.shp");

    std::fs::write(
        &input,
        r#"{
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Polygon", "coordinates":
                        [[[0.0,0.0],[2.0,0.0],[2.0,2.0],[0.0,2.0],[0.0,0.0]]]},
                    "properties": {"name": "alpha", "id": 1}
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Polygon", "coordinates":
                        [[[5.0,5.0],[7.0,5.0],[7.0,7.0],[5.0,7.0],[5.0,5.0]]]},
                    "properties": {"name": "beta", "id": 2}
                }
            ]
        }"#,
    )
    .unwrap();

    let counts = run(&format!(
        r#"
[project]
name = "geojson-to-shapefile"

[[source]]
name = "zones"
format = "geojson"
path = "{input}"

[[sink]]
name = "out"
input = "zones"
format = "shapefile"
path = "{output}"
"#,
        input = input.display(),
        output = output.display()
    ));
    assert_eq!(counts, vec![2, 2]);

    // all sidecars, plus a .prj because the GeoJSON reader reports EPSG:4326
    for ext in ["shp", "shx", "dbf", "prj"] {
        assert!(
            output.with_extension(ext).exists(),
            "missing .{ext} sidecar"
        );
    }
    let prj = std::fs::read_to_string(output.with_extension("prj")).unwrap();
    assert!(prj.contains("4326"), "prj should describe EPSG:4326: {prj}");

    let written = read_shapefile(&output).unwrap();
    assert_eq!(written.len(), 2);
    assert!(matches!(
        written.features[0].geometry,
        FeatureGeometry::Polygon(_)
    ));
    assert_eq!(
        written.features[0].properties.get("name"),
        Some(&Value::String("alpha".into()))
    );
    // dBase stores numbers as fixed point text, so integers come back as floats
    assert_eq!(
        written.features[1].properties.get("id"),
        Some(&Value::Float(2.0))
    );
}

#[test]
fn test_unknown_format_is_rejected() {
    let manifest = Manifest::from_toml(
        r#"
[project]
name = "bad-format"

[[source]]
name = "src"
format = "geotiff"
path = "raster.tif"

[[sink]]
name = "out"
input = "src"
format = "geojson"
path = "out.geojson"
"#,
    )
    .unwrap();

    let err = Pipeline::new(manifest)
        .unwrap()
        .execute(&MultiFormatReader, &default_registry(), &MultiFormatWriter)
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("geotiff"), "{message}");
    assert!(message.contains("geopackage"), "{message}");
}

#[test]
fn test_shapefile_sink_rejects_mixed_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mixed.geojson");

    std::fs::write(
        &input,
        r#"{
            "type": "FeatureCollection",
            "features": [
                {"type": "Feature", "properties": {"id": 1},
                 "geometry": {"type": "Point", "coordinates": [0.0, 0.0]}},
                {"type": "Feature", "properties": {"id": 2},
                 "geometry": {"type": "Polygon", "coordinates":
                    [[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,0.0]]]}}
            ]
        }"#,
    )
    .unwrap();

    let manifest = Manifest::from_toml(&format!(
        r#"
[project]
name = "mixed"

[[source]]
name = "src"
format = "geojson"
path = "{input}"

[[sink]]
name = "out"
input = "src"
format = "shp"
path = "{output}"
"#,
        input = input.display(),
        output = dir.path().join("mixed.shp").display()
    ))
    .unwrap();

    let err = Pipeline::new(manifest)
        .unwrap()
        .execute(&MultiFormatReader, &default_registry(), &MultiFormatWriter)
        .unwrap_err();
    assert!(err.to_string().contains("one geometry type"), "{err}");
    assert!(!Path::new(&dir.path().join("mixed.dbf")).exists());
}

#[test]
fn test_geojson_through_centroid_to_csv_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("zones.geojson");
    let csv = dir.path().join("out").join("centers.csv");
    let geojson_again = dir.path().join("out").join("centers.geojson");

    std::fs::write(
        &input,
        r#"{
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": {"type": "Polygon", "coordinates":
                        [[[0.0,0.0],[3.0,0.0],[3.0,3.0],[0.0,3.0],[0.0,0.0]]]},
                    "properties": {"name": "alpha", "pop": 100, "share": 0.25}
                },
                {
                    "type": "Feature",
                    "geometry": {"type": "Polygon", "coordinates":
                        [[[8.0,8.0],[9.0,8.0],[9.0,9.0],[8.0,9.0],[8.0,8.0]]]},
                    "properties": {"name": "beta", "pop": 250, "share": 0.75}
                }
            ]
        }"#,
    )
    .unwrap();

    let counts = run(&format!(
        r#"
[project]
name = "geojson-to-csv"

[[source]]
name = "zones"
format = "geojson"
path = "{input}"

[[transform]]
name = "centers"
input = "zones"
operation = "centroid"

[[sink]]
name = "out"
input = "centers"
format = "csv"
path = "{csv}"
"#,
        input = input.display(),
        csv = csv.display()
    ));
    assert_eq!(counts, vec![2, 2, 2]);

    let written = std::fs::read_to_string(&csv).unwrap();
    assert_eq!(written.lines().next(), Some("lon,lat,name,pop,share"));
    assert_eq!(written.lines().nth(1), Some("1.5,1.5,alpha,100,0.25"));

    // the csv reads back as a source, so the sink output is a usable input
    let counts = run(&format!(
        r#"
[project]
name = "csv-back-to-geojson"

[[source]]
name = "centers"
format = "csv"
path = "{csv}"

[[sink]]
name = "out"
input = "centers"
format = "geojson"
path = "{geojson}"
"#,
        csv = csv.display(),
        geojson = geojson_again.display()
    ));
    assert_eq!(counts, vec![2, 2]);

    let round_tripped = std::fs::read_to_string(&geojson_again).unwrap();
    assert!(round_tripped.contains("[1.5,1.5]"), "{round_tripped}");
    assert!(round_tripped.contains("alpha"), "{round_tripped}");
    assert!(round_tripped.contains("0.25"), "{round_tripped}");
}

#[test]
fn test_csv_sink_rejects_non_point_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("zones.gpkg");

    write_geopackage(
        &input,
        &FeatureCollection::new(vec![parcel(0.0, "north", 1, 4.0)], None),
        "zones",
    )
    .unwrap();

    let manifest = Manifest::from_toml(&format!(
        r#"
[project]
name = "polygons-to-csv"

[[source]]
name = "zones"
format = "gpkg"
path = "{input}"
layer = "zones"

[[sink]]
name = "out"
input = "zones"
format = "csv"
path = "{output}"
"#,
        input = input.display(),
        output = dir.path().join("out.csv").display()
    ))
    .unwrap();

    let err = Pipeline::new(manifest)
        .unwrap()
        .execute(&MultiFormatReader, &default_registry(), &MultiFormatWriter)
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("cannot write a Polygon"), "{message}");
    assert!(!Path::new(&dir.path().join("out.csv")).exists());
}
