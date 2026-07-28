# Geodukt

[![CI](https://github.com/GeoLang/geodukt/actions/workflows/ci.yml/badge.svg)](https://github.com/GeoLang/geodukt/actions)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

A declarative geospatial ETL pipeline — **dbt for spatial data**.

Define transformations as a DAG of models. Geodukt resolves dependencies, validates geometries, and materializes outputs to your target format.

## Features

- **Declarative pipeline definitions** — TOML manifest files describe sources, transforms, and sinks
- **DAG execution engine** — automatic dependency resolution, parallel where possible
- **Spatial transforms** — reproject, clip, buffer, simplify, spatial join, centroid, dissolve
- **Formats** — pipeline sources and sinks read and write GeoJSON. CSV, GeoPackage, and Shapefile readers exist in `geodukt-io` but are not wired into the pipeline yet
- **Validation** — geometry validity checks, CRS verification, schema assertions
- **Incremental processing** — hash-based change detection, only reprocess what changed
- **Lineage tracking** — full provenance from source to sink
- **Geoprocessing REST service** — HTTP API exposing buffer, centroid, clip, dissolve, and simplify as on-demand tools with JSON I/O (`/gp/catalog` for discovery), plus `/run` and `/runs` for pipeline execution

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

[[source]]
name = "zoning"
format = "geojson"
path = "data/zoning.geojson"

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
clip_to = "zoning"

[[sink]]
name = "output"
input = "clipped_parcels"
format = "geojson"
path = "output/parcels_clipped.geojson"
```

## Architecture

```
geodukt-core    — DAG engine, transform registry, execution scheduler
geodukt-transforms — spatial operations (reproject, clip, buffer, join, etc.)
geodukt-io      — source/sink connectors (GeoJSON wired up, CSV/GeoPackage/Shapefile available)
geodukt-server  — REST API for pipeline runs and geoprocessing tools
geodukt-cli     — command-line interface
```

## License

AGPL-3.0-or-later, see [LICENSE](LICENSE).

Copyright (C) 2026 Grok Image Compression Inc.
