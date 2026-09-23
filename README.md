# Geodukt

[![CI](https://github.com/GeoLang/geodukt/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/geodukt/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

A declarative geospatial ETL pipeline: ten spatial transforms over GeoJSON, GeoPackage, Shapefile and CSV, defined in one TOML manifest.

Geodukt resolves the dependencies between sources, transforms and sinks, runs the graph wave by wave, and writes each sink in the format it names.

## Features

- **TOML manifests** that list sources, transforms and sinks. The order comes from the inputs each node names.
- **DAG execution** in waves of independent nodes. The sources and the local transforms and sinks inside one wave run concurrently under rayon.
- **Transforms**: reproject, clip, buffer, simplify, centroid, dissolve, filter, expression, schema_map, spatial_join. The set is fixed, a manifest cannot name anything else.
- **Formats**: sources and sinks read and write GeoJSON, GeoPackage, Shapefile and CSV.
- **No PROJ, GEOS or GDAL**: geometry comes from [topoi](https://github.com/GeoLang/topoi) and coordinate transforms from [projicio](https://github.com/GeoLang/projicio). The build still needs a C toolchain, because `geodukt-io` and `geodukt-server` use rusqlite with `bundled`, which compiles SQLite.
- **Project flags** on `[project]`, all off by default:
  - `quality = true` fails a transform whose output holds an invalid geometry, including `filter`, `schema_map` and `clip` when they run on geoplumb.
  - `incremental = true` hashes the source files into `.geodukt/incremental.json` and skips the run when none changed.
  - `lineage = true` writes `.geodukt/lineage.json` after a successful run. Feature index mappings are recorded only for operations that keep feature order. Filter, clip and dissolve record node-level provenance.
  - `.geodukt/` is created in the working directory, not beside the manifest.
- **REST API**: `/validate` checks a manifest without running it, `/operations` lists every operation and format a manifest may name, `/run` and `/runs` execute and record runs, and `/gp/*` runs single operations over GeoJSON.

## Quick Start

Geodukt is not on crates.io. A `v*` tag builds `geodukt` for x86_64 and aarch64
Linux and macOS and uploads one tarball per target to
[GitHub Releases](https://github.com/GeoLang/geodukt/releases). The same tag
publishes `ghcr.io/geolang/geodukt`, which runs `geodukt serve --bind
0.0.0.0:8100`. To build from a checkout, run `cargo build --release -p geodukt-cli`.

```bash
# create my-pipeline/ with geodukt.toml, data/ and output/
geodukt init my-pipeline
cd my-pipeline
# the generated manifest reads data/input.geojson, which you supply

# check the manifest and print the execution order
geodukt validate

# print the execution order as a chain
geodukt graph

# run it
geodukt run

# start the REST server
geodukt serve --bind 127.0.0.1:8080

# write pipeline docs, markdown or html, to stdout or --output
geodukt docs --format markdown

# list sources, transforms and sinks added or removed since a git ref
geodukt diff --from HEAD~1
```

Every subcommand but `init` and `serve` reads `geodukt.toml` unless given
`--manifest`. Paths in a manifest resolve against the working directory.

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

A source also accepts a `crs` field. Nothing reads it, and it only appears in
the `/validate` plan. Use a `reproject` transform to change a CRS.

## Formats

`format` on a source or sink takes one of:

| format | aliases | reads | writes |
|--------|---------|-------|--------|
| `csv` | | yes | yes |
| `geojson` | | yes | yes |
| `geopackage` | `gpkg` | yes | yes |
| `shapefile` | `shp` | yes | yes |

GeoPackage sources and sinks take an optional `layer` naming the table. A source
without one reads the first feature table in the file. A sink without one writes
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

- **GeoJSON** holds any geometry type and is assumed to be EPSG:4326.
- **GeoPackage** round-trips geometry, attribute types (integer, float, text, null), and the CRS as an EPSG code.
- **Shapefile** writes the .shp, .shx and .dbf files, plus a .prj when the CRS is a known EPSG code. The format holds one geometry type per file, attribute names up to 10 bytes and field widths up to 254 bytes. A numeric column that holds a fraction is written with 8 decimal places. A collection that breaks any of those limits fails the run instead of being written with data lost.
- **CSV** carries point geometry only, as a `lon,lat` pair followed by one column per property. Writing anything but a point fails the run. Reads accept `lon`/`longitude`/`x` and `lat`/`latitude`/`y`, and assume EPSG:4326. CSV stores no types, so a read infers one per cell: headers come back lowercased, a string that looks like a number comes back as a number, `true`/`false` come back as strings, and an empty cell comes back as an empty string rather than null.

## REST API

`geodukt serve` exposes the pipeline over HTTP, for a UI or an agent that
composes manifests and checks them before running.

| method | path | purpose |
|--------|------|---------|
| GET | `/health` | status and version |
| GET | `/operations` | every operation and format a manifest may name |
| POST | `/validate` | parse and check a manifest, return the plan, run nothing |
| POST | `/run` | execute a manifest and record the run |
| GET | `/runs` | the recorded runs the caller may read |
| GET | `/runs/{id}` | one run, including the manifest it ran |
| GET | `/gp/catalog` | the operations exposed as single tools |
| POST | `/gp/{tool}` | run one operation over GeoJSON in the request body |

| variable | meaning |
|----------|---------|
| `PLATFORM_JWT_SECRET` | HS256 secret shared with the other GeoLang services. Unset or empty turns authentication off. |
| `GEODUKT_RUNS_DB` | sqlite file for the run history. Unset means an in-memory history that a restart empties. |

### GET /operations

The catalog a manifest author works from. Both lists are built from the tables
the engine dispatches on.

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
`default` is the literal TOML value used when the parameter is absent.

A `required` parameter has no default, and the manifest has to supply it.
`geodukt validate`, `geodukt run`, `/validate` and `/run` all reject a transform
that leaves one out, naming the transform, the operation and what the parameter
is for:

```
transform 'wide' uses operation 'buffer' which cannot run: missing required
parameter 'distance' (Buffer distance in meters, negative to shrink a polygon)
```

Required: `buffer.distance`, `simplify.epsilon`, `reproject.to_crs`,
`filter.field`, `filter.equals`, `expression.expressions`, `spatial_join.join`,
and all four edges of `clip`. `schema_map` instead carries `requires_any`: it
needs at least one of `rename`, `drop` and `add`.

`spatial_join` copies properties from the features of a second step, named by
`join`. The DAG treats that step as a second parent, so both inputs run before
the join. `join_type` is `intersects` by default, or `contains` or `within`.

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
| `operation` | 422 | a transform names an operation that does not exist or leaves out a required parameter |
| `format` | 422 | a source or sink names a format geodukt cannot read or write |

### POST /run

Same body as `/validate`. Executes the manifest and records the attempt, whether
it succeeds or fails, so every run is retrievable from `/runs`.

Both outcomes return a run record, and `status` tells them apart:

```json
{"id": 1, "status": "Completed", "manifest_name": "city", "manifest": "<TOML>",
 "steps": [{"name": "parcels", "feature_count": 120, "status": "Completed"}],
 "started_at": "2026-08-12T09:14:02.417Z", "finished_at": "2026-08-12T09:14:05.902Z"}

{"id": 2, "status": {"Failed": "Execution error: sink error for 'out': csv carries point geometry as lon/lat columns, cannot write a Polygon, ..."},
 "manifest_name": "doomed", "manifest": "<TOML>",
 "steps": [{"name": "polys", "feature_count": 1, "status": "Completed"},
           {"name": "out", "feature_count": 0, "status": {"Failed": "sink error for 'out': ..."}},
           {"name": "report", "feature_count": 0, "status": "NotRun"}],
 "started_at": "2026-08-12T09:15:11.003Z", "finished_at": "2026-08-12T09:15:11.244Z"}
```

`started_at` is read before the pipeline starts and `finished_at` when the record
is stored, both RFC 3339 in UTC. A record made with authentication on also
carries the caller's `sub`.

| outcome | status | body |
|---------|--------|------|
| ran to completion | 200 | run record, `status` is `"Completed"` |
| ran and failed | 422 | run record, `status` is `{"Failed": "<reason>"}` |
| invalid TOML, or a graph error | 400 | plain text, nothing recorded |
| a missing required parameter | 422 | `{"kind": "operation", ...}` as from `/validate`, nothing recorded |
| the record could not be stored | 500 | plain text |

A failed run is 422 because the request was well formed and the work it
described could not be done, such as a missing input file or a geometry the sink
format cannot hold. Retrying it will not help. A failed record keeps its steps:
finished steps are `Completed` with their feature counts, the step that failed
carries its error, and steps the run never reached are `NotRun`.

### Authentication

With `PLATFORM_JWT_SECRET` set, `/run` and every `/gp/*` route, `/gp/catalog`
included, accept either a platform JWT with the `editor` or `admin` role, or a
role-free tool JWT with `token_use: "tool"` and `scope: ["geodukt:run"]`. A tool
token never falls back to `role`. An empty or wrong scope array is 403. A
missing, non-array or non-string scope claim, an unknown `token_use`, or a tool
token that carries a role is 401.

`/runs` and `/runs/{id}` need a platform JWT of any role:

| token | sees |
|-------|------|
| role `admin` | every caller's runs |
| any other role, known or not | only runs whose `sub` matches the token's |
| `token_use: "tool"` | nothing, 403 |
| missing, expired, or signed with another secret | nothing, 401 |

Someone else's run answers 404 rather than 403, so a caller cannot probe which
ids exist.

`/health`, `/operations` and `/validate` stay open, so a planner or an eval
harness can call them without a token. With the secret unset nothing is checked
and every caller sees every run.

### GET /runs and GET /runs/{id}

`/runs` returns an array of run records, oldest first, and `/runs/{id}` returns
one, both in the shape `POST /run` returns. Neither takes a parameter.

`GEODUKT_RUNS_DB` names the sqlite file, which is created if missing. The server
needs write access to the file and its directory.

### /gp tools

`GET /gp/catalog` lists the operations exposed as tools: `buffer`, `centroid`,
`clip`, `dissolve` and `simplify`, with the same parameter specs as
`/operations`. `POST /gp/{tool}` takes `{"input": <GeoJSON FeatureCollection or
Feature>, "params": {...}}` and returns `{"tool": ..., "feature_count": ..., "output":
<GeoJSON FeatureCollection>}`. Nothing is recorded.

## Execution

A run walks the DAG one wave at a time, a wave being the nodes whose inputs are
all ready. The head of a pipeline that
[geoplumb](https://github.com/GeoLang/geoplumb) can run goes onto a geoplumb pull
graph instead of the in-memory transforms: a source whose next operation is
`filter`, `schema_map` or `clip` becomes a geoplumb source, those operations
become geoplumb elements, and the features come back at the first node geoplumb
cannot run, pulled over the whole extent and merged back into whole features.
Feature counts per step are the same either way. A source with no such operation
under it does not use geoplumb.

Both paths run the same geometry code. `clip` intersects polygons, cuts lines at
the boundary and drops points outside it on either path.

## Crates

```
geodukt-core        DAG, wave scheduler, routing onto geoplumb
geodukt-transforms  spatial operations and the operation registry
geodukt-io          GeoJSON, GeoPackage, Shapefile and CSV readers and writers
geodukt-server      REST API for validation, runs and /gp tools
geodukt-cli         the geodukt binary
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
