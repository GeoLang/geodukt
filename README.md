# Geodukt

A declarative geospatial ETL pipeline — **dbt for spatial data**.

Define transformations as a DAG of models. Geodukt resolves dependencies, validates geometries, and materializes outputs to your target format.

## Features

- **Declarative pipeline definitions** — TOML manifest files describe sources, transforms, and sinks
- **DAG execution engine** — automatic dependency resolution, parallel where possible
- **Spatial transforms** — reproject, clip, buffer, simplify, spatial join, centroid, dissolve
- **Multiple formats** — GeoJSON, CSV (lon/lat), GeoPackage, FlatGeobuf, GeoParquet
- **Validation** — geometry validity checks, CRS verification, schema assertions
- **Incremental processing** — hash-based change detection, only reprocess what changed
- **Lineage tracking** — full provenance from source to sink
- **Geoprocessing REST service** — HTTP API exposing buffer, centroid, clip, dissolve, and simplify as on-demand tools with JSON I/O (`/gp/catalog` for discovery)

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
format = "geoparquet"
path = "output/parcels_clipped.parquet"
```

## Architecture

```
geodukt-core    — DAG engine, transform registry, execution scheduler
geodukt-transforms — spatial operations (reproject, clip, buffer, join, etc.)
geodukt-io      — source/sink connectors (GeoJSON, CSV, GeoPackage, Parquet)
geodukt-cli     — command-line interface
```

## License

AGPL-3.0-or-later
