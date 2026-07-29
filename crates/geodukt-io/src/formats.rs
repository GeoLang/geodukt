//! Format dispatch — maps the `format` field of a manifest source or sink to a
//! reader or writer.

use std::fs;
use std::path::Path;

use geodukt_core::feature::FeatureCollection;
use geodukt_core::manifest::{DEFAULT_LAYER, Sink, Source};
use geodukt_core::pipeline::{PipelineError, SinkWriter, SourceReader};

use crate::csv_io::{CsvReader, CsvWriter};
use crate::geojson_io::{GeoJsonReader, GeoJsonWriter};
use crate::geopackage_io::{read_geopackage, write_geopackage};
use crate::shapefile_io::{read_shapefile, write_shapefile};

/// Formats a manifest source or sink may name, with the aliases accepted for each.
const SUPPORTED_FORMATS: &str = "csv, geojson, geopackage (gpkg), shapefile (shp)";

/// Multi-format reader that delegates to format-specific readers.
pub struct MultiFormatReader;

impl SourceReader for MultiFormatReader {
    fn read_source(&self, source: &Source) -> Result<FeatureCollection, PipelineError> {
        let path = Path::new(&source.path);
        match source.format.as_str() {
            "csv" => CsvReader.read_source(source),
            "geojson" => GeoJsonReader.read_source(source),
            "geopackage" | "gpkg" => read_geopackage(path, source.layer.as_deref()),
            "shapefile" | "shp" => read_shapefile(path),
            other => Err(PipelineError::Source {
                name: source.name.clone(),
                message: format!(
                    "unsupported format '{other}', expected one of {SUPPORTED_FORMATS}"
                ),
            }),
        }
    }
}

/// Multi-format writer that delegates to format-specific writers.
pub struct MultiFormatWriter;

impl SinkWriter for MultiFormatWriter {
    fn write_sink(&self, data: &FeatureCollection, sink: &Sink) -> Result<(), PipelineError> {
        let path = Path::new(&sink.path);
        match sink.format.as_str() {
            "csv" => CsvWriter.write_sink(data, sink),
            "geojson" => GeoJsonWriter.write_sink(data, sink),
            "geopackage" | "gpkg" => {
                write_geopackage(path, data, sink.layer.as_deref().unwrap_or(DEFAULT_LAYER))
            }
            "shapefile" | "shp" => write_shapefile(path, data),
            other => Err(PipelineError::Sink {
                name: sink.name.clone(),
                message: format!(
                    "unsupported format '{other}', expected one of {SUPPORTED_FORMATS}"
                ),
            }),
        }
    }
}

/// Create the directory a sink writes into, so a manifest can name an output
/// folder that does not exist yet. Each writer calls this for itself.
pub(crate) fn create_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}
