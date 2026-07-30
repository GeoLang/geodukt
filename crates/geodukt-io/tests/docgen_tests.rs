//! Tests for documentation generation.

use geodukt_core::manifest::Manifest;
use geodukt_io::docgen;

const SAMPLE_MANIFEST: &str = r#"
[project]
name = "test-pipeline"
version = "1.0.0"

[[source]]
name = "input"
format = "geojson"
path = "data/input.geojson"

[[transform]]
name = "buffered"
input = "input"
operation = "buffer"
distance = 100.0

[[transform]]
name = "filtered"
input = "buffered"
operation = "filter"
field = "type"
equals = "road"

[[sink]]
name = "output"
input = "filtered"
format = "geojson"
path = "output/result.geojson"
"#;

#[test]
fn test_generate_markdown_has_sources() {
    let manifest = Manifest::from_toml(SAMPLE_MANIFEST).unwrap();
    let md = docgen::generate_markdown(&manifest);
    assert!(md.contains("# test-pipeline"));
    assert!(md.contains("**Version:** 1.0.0"));
    assert!(md.contains("| input | geojson | data/input.geojson |"));
}

#[test]
fn test_generate_markdown_has_transforms() {
    let manifest = Manifest::from_toml(SAMPLE_MANIFEST).unwrap();
    let md = docgen::generate_markdown(&manifest);
    assert!(md.contains("| buffered | input | buffer |"));
    assert!(md.contains("| filtered | buffered | filter |"));
}

#[test]
fn test_generate_markdown_has_sinks() {
    let manifest = Manifest::from_toml(SAMPLE_MANIFEST).unwrap();
    let md = docgen::generate_markdown(&manifest);
    assert!(md.contains("| output | geojson | output/result.geojson |"));
}

#[test]
fn test_generate_markdown_has_mermaid_dag() {
    let manifest = Manifest::from_toml(SAMPLE_MANIFEST).unwrap();
    let md = docgen::generate_markdown(&manifest);
    assert!(md.contains("```mermaid"));
    assert!(md.contains("graph LR"));
    assert!(md.contains("input --> |buffer| buffered"));
    assert!(md.contains("buffered --> |filter| filtered"));
    assert!(md.contains("filtered --> output"));
}

#[test]
fn test_generate_html_wraps_content() {
    let manifest = Manifest::from_toml(SAMPLE_MANIFEST).unwrap();
    let html = docgen::generate_html(&manifest);
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("<title>test-pipeline"));
    assert!(html.contains("# test-pipeline"));
}

#[test]
fn test_generate_markdown_names_geopackage_layer() {
    let manifest = Manifest::from_toml(
        r#"
[project]
name = "gpkg-docs"

[[source]]
name = "parcels"
format = "geopackage"
path = "data/city.gpkg"
layer = "parcels"

[[sink]]
name = "out"
input = "parcels"
format = "geopackage"
path = "out/city.gpkg"
layer = "parcels_centroids"
"#,
    )
    .unwrap();
    let md = docgen::generate_markdown(&manifest);
    assert!(md.contains("| parcels | geopackage | data/city.gpkg (layer: parcels) |"));
    assert!(md.contains("| out | geopackage | out/city.gpkg (layer: parcels_centroids) |"));
}
