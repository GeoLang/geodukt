# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `POST /validate`: check a manifest without running it, returning the step
  order and per-step details, or a problem tagged `toml`, `graph`, `operation`,
  or `format`
- `GET /operations`: catalog of every transform operation and source/sink format
  a manifest may name, generated from the tables the engine dispatches on
- Run records keep the manifest TOML, so `GET /runs/{id}` is enough to repeat a run

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

### Fixed
- `/gp/dissolve` and `/gp/simplify` passed `field` and `tolerance` while the
  transforms read `group_by` and `epsilon`, so both parameters were ignored. The
  endpoints now take the names the transforms read, and `/gp/catalog` is
  generated from the registry table instead of a hand-written copy
- The README pipeline example set `clip_to` on a clip transform, which nothing
  reads. It now uses the bounding box parameters clip actually takes
