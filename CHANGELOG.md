# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- `Pipeline::execute` runs the engine-mappable head of a pipeline on a
  [geoplumb](https://github.com/GeoLang/geoplumb) pull graph. A source whose
  data the engine can hold and whose next operation is `filter`, `schema_map`
  or `clip` becomes a `VecSrc`, that run of operations becomes elements, and
  the features come back at the first node the engine cannot run: a
  full-extent pull whose tile fragments are dissolved into whole features.
  A source feeding only sinks or operations the engine does not run stays off
  it, as does an operation the caller never registered, which still fails the
  run as an unknown operation. Manifests, the CLI and the REST API are
  unchanged, and an operation reports the same per-step feature count wherever
  it runs
- Breaking: `clip` cuts lines where they cross the boundary and drops points
  outside it, where it used to pass both through untouched, so a clip over
  lines or points now reports fewer features and less geometry than it did.
  `ClipTransform` and the engine's clip both call topoi's `clip_to_boundary`,
  so a chain gets the same answer whether or not the engine ran it, and
  `POST /gp/clip` cuts the same way. A multi-polygon boundary is honoured
  whole, where the old clip used only its first polygon
- A polygon wide enough to cross engine tile seams comes back geometrically
  equal but not vertex for vertex: merging its fragments rebuilds the rings,
  which drops collinear vertices
- Geometry now comes from topoi and coordinate transforms from projicio, both
  pure Rust, replacing the `geo`, `proj` and `geozero` crates. `Feature.geometry`
  is a `topoi_core::geojson::FeatureGeometry`, which drops the `Line`, `Rect` and
  `Triangle` variants a reader never produced and adds `GeometryCollection` to
  the GeoPackage writer. Building geodukt no longer needs a PROJ system library
  or its CRS database, so the `reproject` cargo feature is gone and reproject and
  buffer are always available
- Breaking: a transform that leaves out a parameter its operation cannot run
  without is rejected by `/validate`, `/run` and the CLI, naming the transform,
  the operation and what the parameter is for. `buffer.distance`,
  `simplify.epsilon`, `reproject.to_crs`, `filter.field`, `filter.equals`,
  `expression.expressions` and all four `clip` edges are required and no longer
  carry a default, and `schema_map` needs at least one of `rename`, `drop` and
  `add`. A manifest that relied on one of those defaults now fails instead of
  producing output nobody asked for

### Added
- `POST /validate`: check a manifest without running it, returning the step
  order and per-step details, or a problem tagged `toml`, `graph`, `operation`,
  or `format`
- `GET /operations`: catalog of every transform operation and source/sink format
  a manifest may name, generated from the tables the engine dispatches on
- Run records keep the manifest TOML, so `GET /runs/{id}` is enough to repeat a run
- Failed runs are recorded like completed ones, so `/runs` and `/runs/{id}` show
  them. `POST /run` answers a failure with 422 and the run record, whose `status`
  is `{"Failed": "<reason>"}`. The success response is unchanged

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
