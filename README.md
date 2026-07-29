# Geodukt

[![CI](https://github.com/GeoLang/geodukt/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/geodukt/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

A declarative geospatial ETL pipeline — **dbt for spatial data**.

Define transformations as a DAG of models. Geodukt resolves dependencies, validates geometries, and materializes outputs to your target format.

## Features

- **Declarative pipeline definitions** — TOML manifest files describe sources, transforms, and sinks
- **DAG execution engine** — automatic dependency resolution, parallel where possible
- **Spatial transforms** — reproject, clip, buffer, simplify, centroid, dissolve, filter, expression, schema map
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
        {"name": "epsilon", "param_type": "float", "required": false,
         "default": "0.001", "description": "Douglas-Peucker tolerance in CRS units, larger removes more vertices"}
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
parameter with a default is optional. An operation carrying an `unavailable`
field cannot run from a manifest, and `/validate` rejects it: `spatial_join` is
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
| `operation` | 422 | a transform names an operation that does not exist or cannot run |
| `format` | 422 | a source or sink names a format that is not wired up |

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
