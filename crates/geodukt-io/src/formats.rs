//! Format dispatch — maps the `format` field of a manifest source or sink to a
//! reader or writer.
//!
//! [`formats`] is the one table of what a manifest may name. Both dispatchers
//! resolve through it, so aliases and the error message listing the choices
//! cannot drift from what is actually wired up.

use std::fs;
use std::path::Path;

use geodukt_core::feature::FeatureCollection;
use geodukt_core::manifest::{DEFAULT_LAYER, Sink, Source};
use geodukt_core::pipeline::{PipelineError, SinkWriter, SourceReader};
use serde::Serialize;

use crate::csv_io::{CsvReader, CsvWriter};
use crate::geojson_io::{GeoJsonReader, GeoJsonWriter};
use crate::geopackage_io::{read_geopackage, write_geopackage};
use crate::shapefile_io::{read_shapefile, write_shapefile};

/// One format a manifest source or sink may name.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FormatSpec {
    /// Canonical name, the one this table dispatches on.
    pub name: &'static str,
    /// Other spellings accepted for `name`.
    pub aliases: &'static [&'static str],
    pub reads: bool,
    pub writes: bool,
    /// Manifest fields this format uses, beyond `name` and `input`.
    pub fields: &'static [&'static str],
    pub description: &'static str,
}

const FORMATS: &[FormatSpec] = &[
    FormatSpec {
        name: "csv",
        aliases: &[],
        reads: true,
        writes: true,
        fields: &["path"],
        description: "Point geometry as lon/lat columns, one column per property, assumed EPSG:4326",
    },
    FormatSpec {
        name: "geojson",
        aliases: &[],
        reads: true,
        writes: true,
        fields: &["path"],
        description: "GeoJSON FeatureCollection, any geometry type, assumed EPSG:4326",
    },
    FormatSpec {
        name: "geopackage",
        aliases: &["gpkg"],
        reads: true,
        writes: true,
        fields: &["path", "layer"],
        description: "GeoPackage layer, keeps geometry, attribute types, and the CRS",
    },
    FormatSpec {
        name: "shapefile",
        aliases: &["shp"],
        reads: true,
        writes: true,
        fields: &["path"],
        description: "Shapefile with .shx/.dbf/.prj sidecars, one geometry type per file",
    },
];

/// Every format a manifest may name, sorted by name.
pub fn formats() -> &'static [FormatSpec] {
    FORMATS
}

/// Resolve a manifest `format` value, by canonical name or alias.
pub fn format(name: &str) -> Option<&'static FormatSpec> {
    FORMATS
        .iter()
        .find(|f| f.name == name || f.aliases.contains(&name))
}

/// The formats and aliases a manifest may name, for an error message.
fn supported_formats() -> String {
    FORMATS
        .iter()
        .map(|f| {
            if f.aliases.is_empty() {
                f.name.to_string()
            } else {
                format!("{} ({})", f.name, f.aliases.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn unsupported(format: &str) -> String {
    format!(
        "unsupported format '{format}', expected one of {}",
        supported_formats()
    )
}

/// Multi-format reader that delegates to format-specific readers.
pub struct MultiFormatReader;

impl SourceReader for MultiFormatReader {
    fn read_source(&self, source: &Source) -> Result<FeatureCollection, PipelineError> {
        let path = Path::new(&source.path);
        match format(&source.format).map(|f| f.name) {
            Some("csv") => CsvReader.read_source(source),
            Some("geojson") => GeoJsonReader.read_source(source),
            Some("geopackage") => read_geopackage(path, source.layer.as_deref()),
            Some("shapefile") => read_shapefile(path),
            _ => Err(PipelineError::Source {
                name: source.name.clone(),
                message: unsupported(&source.format),
            }),
        }
    }
}

/// Multi-format writer that delegates to format-specific writers.
pub struct MultiFormatWriter;

impl SinkWriter for MultiFormatWriter {
    fn write_sink(&self, data: &FeatureCollection, sink: &Sink) -> Result<(), PipelineError> {
        let path = Path::new(&sink.path);
        match format(&sink.format).map(|f| f.name) {
            Some("csv") => CsvWriter.write_sink(data, sink),
            Some("geojson") => GeoJsonWriter.write_sink(data, sink),
            Some("geopackage") => {
                write_geopackage(path, data, sink.layer.as_deref().unwrap_or(DEFAULT_LAYER))
            }
            Some("shapefile") => write_shapefile(path, data),
            _ => Err(PipelineError::Sink {
                name: sink.name.clone(),
                message: unsupported(&sink.format),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn source(format: &str) -> Source {
        Source {
            name: "src".into(),
            format: format.into(),
            path: "/nonexistent/geodukt-dispatch-probe".into(),
            crs: None,
            layer: None,
        }
    }

    fn sink(format: &str) -> Sink {
        Sink {
            name: "out".into(),
            input: "src".into(),
            format: format.into(),
            path: "/nonexistent/geodukt-dispatch-probe".into(),
            layer: None,
        }
    }

    /// Every name and alias in the table has to reach a dispatch arm. A row with
    /// no arm would fall through to the unsupported error at runtime, which is
    /// exactly the drift the table exists to prevent. The probe path cannot be
    /// read or written, so whether the call fails does not matter here, only
    /// that it failed for some reason other than an unresolved format.
    #[test]
    fn test_every_table_entry_reaches_the_dispatch() {
        let empty = FeatureCollection::empty();
        for spec in formats() {
            for name in std::iter::once(&spec.name).chain(spec.aliases) {
                if spec.reads
                    && let Err(err) = MultiFormatReader.read_source(&source(name))
                {
                    assert!(
                        !err.to_string().contains("unsupported format"),
                        "reader has no arm for '{name}'"
                    );
                }
                if spec.writes
                    && let Err(err) = MultiFormatWriter.write_sink(&empty, &sink(name))
                {
                    assert!(
                        !err.to_string().contains("unsupported format"),
                        "writer has no arm for '{name}'"
                    );
                }
            }
        }
    }

    #[test]
    fn test_unknown_format_lists_the_table() {
        let err = MultiFormatReader
            .read_source(&source("geotiff"))
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("unsupported format 'geotiff'"),
            "{message}"
        );
        for spec in formats() {
            assert!(message.contains(spec.name), "{message} omits {}", spec.name);
        }
        assert!(
            message.contains("gpkg") && message.contains("shp"),
            "{message}"
        );
    }

    #[test]
    fn test_resolves_aliases_to_canonical_names() {
        assert_eq!(format("gpkg").unwrap().name, "geopackage");
        assert_eq!(format("shp").unwrap().name, "shapefile");
        assert_eq!(format("geopackage").unwrap().name, "geopackage");
        assert!(format("GPKG").is_none(), "format names are case sensitive");
        assert!(format("geotiff").is_none());
    }

    #[test]
    fn test_layer_is_declared_only_where_it_is_read() {
        for spec in formats() {
            assert_eq!(
                spec.fields.contains(&"layer"),
                spec.name == "geopackage",
                "{} declares the wrong fields",
                spec.name
            );
        }
    }

    #[test]
    fn test_formats_are_sorted_and_unique() {
        let names: Vec<&str> = formats().iter().map(|f| f.name).collect();
        let mut expected = names.clone();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(names, expected);
    }
}
