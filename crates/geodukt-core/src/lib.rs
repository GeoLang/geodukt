//! # geodukt-core
//!
//! Core DAG execution engine and pipeline model for geospatial ETL.

pub mod cache;
pub mod cdc;
pub mod dag;
pub mod feature;
pub mod geometry;
pub mod incremental;
pub mod lineage;
pub mod manifest;
pub mod pipeline;
pub mod quality;
pub mod routing;
pub mod scheduler;
pub mod schema;
pub mod streaming;
pub mod template;
pub mod testing;
pub mod visual;
