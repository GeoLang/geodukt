# Geodukt

[![CI](https://github.com/GeoLang/geodukt/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/geodukt/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

A declarative geospatial ETL pipeline — **dbt for spatial data**.

Define transformations as a DAG of models. Geodukt resolves dependencies, validates geometries, and materializes outputs to your target format.

## Features

- **Declarative pipeline definitions** — TOML manifest files describe sources, transforms, and sinks
- **DAG execution engine** — automatic dependency resolution, parallel where possible
- **Spatial transforms** — reproject, clip, buffer, simplify, centroid, dissolve, filter, expression, schema map
- **Pure Rust**: geometry through [topoi](https://github.com/GeoLang/topoi), coordinate transforms through [projicio](https://github.com/GeoLang/projicio), so a build needs no PROJ or GEOS on the machine
- **Formats** — pipeline sources and sinks read and write GeoJSON, GeoPackage, Shapefile, and CSV
- **Validation** — geometry validity checks, CRS verification, schema assertions
- **Incremental processing** — hash-based change detection, only reprocess what changed
- **Lineage tracking** — full provenance from source to sink
- **REST API** — `/validate` checks a manifest without running it, `/operations` describes every operation and format a manifest may name, `/run` and `/runs` execute and record runs, and `/gp/*` exposes individual tools with JSON I/O

## Quick Start

```bash
# Install
cargo install geodukt-cli

# Initialize a project
geodukt init my-pipeline

# Run the pipeline
geodukt run

# Validate without executing
geodukt validate

# Show the DAG
geodukt graph

# Start the geoprocessing REST server
geodukt serve --bind 127.0.0.1:8080

# Generate pipeline docs (markdown or html)
geodukt docs --format markdown

# Diff pipeline outputs against a git ref
geodukt diff --from HEAD~1
```

## Pipeline Definition

```toml
# geodukt.toml
[project]
name = "city-analysis"
version = "0.1.0"

[[source]]
name = "parcels"
format = "geojson"
path = "data/parcels.geojson"

[[transform]]
name = "parcels_reprojected"
input = "parcels"
operation = "reproject"
from_crs = "EPSG:4326"
to_crs = "EPSG:3857"

[[transform]]
name = "clipped_parcels"
input = "parcels_reprojected"
operation = "clip"
min_x = -13639000.0
min_y = 4536000.0
max_x = -13630000.0
max_y = 4545000.0

[[sink]]
name = "output"
input = "clipped_parcels"
format = "geojson"
path = "output/parcels_clipped.geojson"
```

## Formats

`format` on a source or sink takes one of:

| format | aliases | reads | writes |
|--------|---------|-------|--------|
| `csv` | | yes | yes |
| `geojson` | | yes | yes |
| `geopackage` | `gpkg` | yes | yes |
| `shapefile` | `shp` | yes | yes |

GeoPackage sources and sinks take an optional `layer` naming the table. A source
without one reads the first feature table in the file, a sink without one writes
to `features`.

```toml
[[source]]
name = "parcels"
format = "geopackage"
path = "data/city.gpkg"
layer = "parcels"

[[sink]]
name = "centroids"
input = "parcels_centroids"
format = "geopackage"
path = "output/city.gpkg"
layer = "parcels_centroids"

[[sink]]
name = "export"
input = "parcels_centroids"
format = "shapefile"
path = "output/centroids.shp"
```

What each format carries:

- **GeoPackage** round-trips geometry, attribute types (integer, float, text, null), and the CRS as an EPSG code.
- **Shapefile** writes the .shp, .shx and .dbf sidecars, plus a .prj when the CRS is a known EPSG code. The format holds one geometry type per file, limits attribute names to 10 bytes and field widths to 254 bytes, and stores numbers as fixed point text with 8 decimal places. A collection that breaks any of those rules fails the run instead of being written mangled.
- **CSV** carries point geometry only, as a `lon,lat` pair followed by one column per property. Writing anything but a point fails the run. Reads accept `lon`/`longitude`/`x` and `lat`/`latitude`/`y`, and assume EPSG:4326. Because CSV stores no types, a read infers one per cell: headers come back lowercased, a string that looks like a number comes back as a number, `true`/`false` come back as strings, and an empty cell comes back as an empty string rather than null.

## REST API

`geodukt serve` exposes the pipeline over HTTP. Useful when something else, a UI
or an agent, composes manifests and wants to check them before running.

| method | path | purpose |
|--------|------|---------|
| GET | `/health` | liveness and version |
| GET | `/operations` | every operation and format a manifest may name |
| POST | `/validate` | parse and check a manifest, return the plan, run nothing |
| POST | `/run` | execute a manifest and record the run |
| GET | `/runs` | every recorded run |
| GET | `/runs/{id}` | one run, including the manifest it ran |
| GET | `/gp/catalog` | the subset of operations exposed as one-shot tools |
| POST | `/gp/{tool}` | run one operation over GeoJSON in the request body |

### GET /operations

The catalog a manifest author works from. Both lists are generated from the
tables the engine dispatches on, so they cannot drift from what actually runs.

```json
{
  "operations": [
    {
      "name": "simplify",
      "description": "Reduce vertex count with Douglas-Peucker",
      "parameters": [
        {"name": "epsilon", "param_type": "float", "required": true,
         "description": "Douglas-Peucker tolerance in CRS units, larger removes more vertices"}
      ]
    }
  ],
  "formats": [
    {"name": "geopackage", "aliases": ["gpkg"], "reads": true, "writes": true,
     "fields": ["path", "layer"], "description": "GeoPackage layer, keeps geometry, attribute types, and the CRS"}
  ]
}
```

`param_type` is one of `float`, `integer`, `string`, `table`, `array`, `any`.
`default` is the literal TOML value used when the parameter is absent, so every
parameter with a default is optional.

A `required` parameter is one the operation cannot stand in a value for, so it
carries no default and the manifest has to supply it. `/validate` and `/run`
both reject a transform that leaves one out, naming the transform, the operation
and what the parameter is for:

```
transform 'wide' uses operation 'buffer' which cannot run: missing required
parameter 'distance' (Buffer distance in meters, negative to shrink a polygon)
```

Required today: `buffer.distance`, `simplify.epsilon`, `reproject.to_crs`,
`filter.field`, `filter.equals`, `expression.expressions`, and all four edges of
`clip`, which takes the whole box or none of it. `schema_map` instead carries
`requires_any`, a group it needs at least one member of, because a schema map
that renames, drops and adds nothing does nothing.

An operation carrying an `unavailable` field
cannot run from a manifest, and `/validate` rejects it: `spatial_join` is
in that state because a transform receives a single input, so a manifest has no
way to name the second dataset to join against.

### POST /validate

Takes the same body as `/run`, `{"manifest": "<TOML>"}`. Touches no files and
records no run. Success returns the steps in the order the executor would run
them:

```json
{
  "project": "city",
  "version": "1.0.0",
  "steps": [
    {"name": "parcels", "kind": "source", "format": "gpkg", "path": "data/city.gpkg", "layer": "parcels"},
    {"name": "centers", "kind": "transform", "operation": "centroid", "input": "parcels", "params": {}},
    {"name": "out", "kind": "sink", "input": "centers", "format": "shp", "path": "out/centers.shp"}
  ]
}
```

Fields that do not apply to a step's kind are left out rather than sent as null.

A rejected manifest returns `{"kind": ..., "message": ...}`, where `kind` says
which part to fix:

| kind | status | meaning |
|------|--------|---------|
| `toml` | 400 | not valid TOML, or does not match the manifest schema |
| `graph` | 422 | unknown input, duplicate node name, or a cycle |
| `operation` | 422 | a transform names an operation that does not exist, cannot run, or leaves out a required parameter |
| `format` | 422 | a source or sink names a format that is not wired up |

### POST /run

Same body as `/validate`. Executes the manifest and records the attempt, whether
it succeeds or not, so every run is retrievable from `/runs`.

When `PLATFORM_JWT_SECRET` is set, `/run` requires a platform JWT with the
editor or admin role and records the caller's `sub` on the run. `/validate`,
`/operations` and `/health` stay open. Unset means no gate, the standalone
single-user flow.

Both outcomes return a run record, so a caller parses one shape either way. The
`status` field tells them apart:

```json
{"id": 0, "status": "Completed", "manifest_name": "city", "manifest": "<TOML>",
 "steps": [{"name": "parcels", "feature_count": 120, "status": "Completed"}]}

{"id": 1, "status": {"Failed": "Execution error: sink error for 'out': csv carries point geometry as lon/lat columns, cannot write a Polygon, ..."},
 "manifest_name": "doomed", "manifest": "<TOML>",
 "steps": [{"name": "polys", "feature_count": 1, "status": "Completed"},
           {"name": "out", "feature_count": 0, "status": {"Failed": "sink error for 'out': ..."}},
           {"name": "report", "feature_count": 0, "status": "NotRun"}]}
```

| outcome | status | body |
|---------|--------|------|
| ran to completion | 200 | run record, `status` is `"Completed"` |
| ran and failed | 422 | run record, `status` is `{"Failed": "<reason>"}` |
| not a usable manifest | 400 | plain text, nothing recorded |

A failed run is 422 rather than 500 because the manifest parsed and its graph was
sound, so what failed is the work the request described: a missing input, an
unwritable output path, or geometry the chosen format cannot carry. A 500 would
tell a client the server misbehaved and the request is worth retrying, when it is
not. A 400 is reserved for a body that never became a pipeline, and that case
records nothing because no run was attempted.

A failed record keeps its steps: the ones that finished are `Completed` with
their feature counts, the one that died carries its own error, and the ones the
run never reached are `NotRun`. Records stored before steps had a status read
back as `Completed`.

## Execution

A run walks the DAG in topological order. The head of a pipeline that
[geoplumb](https://github.com/GeoLang/geoplumb) can run goes onto a pull graph
instead of the in-memory transforms: a source whose next operation is `filter`,
`schema_map` or `clip` becomes an engine source, that run of operations becomes
engine elements, and the features come back at the first node the engine cannot
run, pulled over the whole extent and merged back into whole features. Feature
counts per step are the same either way, and a source with nothing mappable
under it never goes near the engine.

Both paths run the same geometry code, so an operation means the same thing
wherever a chain happens to run it. `clip` intersects polygons, cuts lines at
the boundary and drops points outside it, on the engine and off it alike.

## Architecture

```
geodukt-core    — DAG engine, transform registry, execution scheduler
geodukt-transforms — spatial operations (reproject, clip, buffer, dissolve, etc.)
geodukt-io      — source/sink connectors (GeoJSON, GeoPackage, Shapefile, CSV)
geodukt-server  — REST API for validation, pipeline runs, and geoprocessing tools
geodukt-cli     — command-line interface
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
