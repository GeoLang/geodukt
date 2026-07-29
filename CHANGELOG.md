# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Declarative pipeline manifest (geodukt.toml) with TOML parsing
- DAG execution engine with topological sorting (petgraph)
- Pipeline executor with pluggable source/transform/sink traits
- Feature collection data model with geometry + properties
- Buffer transform (bounding rect expansion)
- Centroid transform (geometry → centroid point)
- Filter transform (property-based feature filtering)
- Transform registry with named operation lookup
- GeoJSON source reader and sink writer
- GeoPackage source and sink, with a `layer` parameter, geometry as GeoPackage
  binary, typed attribute columns, and CRS round trips
- Shapefile source and sink, writing the .shp/.shx/.dbf sidecars and a .prj,
  and rejecting collections a shapefile cannot represent
- CSV source and sink, carrying point geometry as lon/lat columns and rejecting
  geometry the format cannot hold
- CLI tool: `run`, `validate`, `graph`, `init` subcommands
- GitHub Actions CI (Ubuntu, Windows, macOS)
- AGPL-3.0-or-later license
