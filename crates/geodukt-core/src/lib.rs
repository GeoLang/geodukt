//! # geodukt-core
//!
//! Core DAG execution engine and pipeline model for geospatial ETL.

pub mod dag;
pub mod feature;
pub mod geometry;
mod hex;
pub mod incremental;
pub mod lineage;
pub mod manifest;
pub mod pipeline;
pub mod quality;
pub mod routing;
pub mod scheduler;
pub mod schema;
