//! Transform registry — the one table of operations the pipeline engine can run.
//!
//! [`default_registry`] builds the executor's dispatch map from [`operations`],
//! and the server's catalog endpoint reads the same table, so what the API
//! advertises and what the engine runs cannot drift apart. Adding an operation
//! means adding one row here.

use std::collections::HashMap;

use geodukt_core::pipeline::TransformOp;
use serde::Serialize;

use crate::buffer::BufferTransform;
use crate::centroid::CentroidTransform;
use crate::clip::ClipTransform;
use crate::dissolve::DissolveTransform;
use crate::expression::ExpressionTransform;
use crate::filter::FilterTransform;
#[cfg(feature = "reproject")]
use crate::reproject::ReprojectTransform;
use crate::schema::SchemaMapTransform;
use crate::simplify::SimplifyTransform;
use crate::spatial_join::SpatialJoinTransform;

/// One parameter of a transform operation, as the operation actually reads it.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParamSpec {
    pub name: &'static str,
    /// One of `float`, `integer`, `string`, `table`, `array`, `any`.
    pub param_type: &'static str,
    /// True only when the operation fails without it.
    pub required: bool,
    /// The literal TOML value used when the parameter is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<&'static str>,
    pub description: &'static str,
}

/// One operation, its parameters, and how to construct it.
#[derive(Clone, Copy, Serialize)]
pub struct OperationSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: &'static [ParamSpec],
    /// Why the pipeline cannot run this operation yet, when it cannot. An
    /// operation with a reason here is listed but rejected by validation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<&'static str>,
    #[serde(skip)]
    build: fn() -> Box<dyn TransformOp>,
}

impl std::fmt::Debug for OperationSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationSpec")
            .field("name", &self.name)
            .field("parameters", &self.parameters)
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

impl OperationSpec {
    /// True when a manifest may use this operation.
    pub fn is_available(&self) -> bool {
        self.unavailable.is_none()
    }
}

const BUFFER: OperationSpec = OperationSpec {
    name: "buffer",
    description: "Expand or shrink geometries by a distance in meters, with round joins",
    parameters: &[
        ParamSpec {
            name: "distance",
            param_type: "float",
            required: false,
            default: Some("1.0"),
            description: "Buffer distance in meters, negative to shrink a polygon",
        },
        ParamSpec {
            name: "segments",
            param_type: "integer",
            required: false,
            default: Some("64"),
            description: "Segments per full circle when approximating round joins, minimum 3",
        },
    ],
    unavailable: None,
    build: || Box::new(BufferTransform),
};

const CENTROID: OperationSpec = OperationSpec {
    name: "centroid",
    description: "Replace each geometry with its centroid point",
    parameters: &[],
    unavailable: None,
    build: || Box::new(CentroidTransform),
};

const CLIP: OperationSpec = OperationSpec {
    name: "clip",
    description: "Intersect geometries with a bounding box, dropping what falls outside",
    parameters: &[
        ParamSpec {
            name: "min_x",
            param_type: "float",
            required: false,
            default: Some("-180.0"),
            description: "West edge of the clip box in CRS units",
        },
        ParamSpec {
            name: "min_y",
            param_type: "float",
            required: false,
            default: Some("-90.0"),
            description: "South edge of the clip box in CRS units",
        },
        ParamSpec {
            name: "max_x",
            param_type: "float",
            required: false,
            default: Some("180.0"),
            description: "East edge of the clip box in CRS units",
        },
        ParamSpec {
            name: "max_y",
            param_type: "float",
            required: false,
            default: Some("90.0"),
            description: "North edge of the clip box in CRS units",
        },
    ],
    unavailable: None,
    build: || Box::new(ClipTransform::new()),
};

const DISSOLVE: OperationSpec = OperationSpec {
    name: "dissolve",
    description: "Group features by a property value and union each group's geometry",
    parameters: &[ParamSpec {
        name: "group_by",
        param_type: "string",
        required: false,
        default: Some("\"\""),
        description: "Property to group by, empty unions every feature into one",
    }],
    unavailable: None,
    build: || Box::new(DissolveTransform),
};

const EXPRESSION: OperationSpec = OperationSpec {
    name: "expression",
    description: "Add computed property columns from geometry measures and arithmetic",
    parameters: &[ParamSpec {
        name: "expressions",
        param_type: "table",
        required: false,
        default: None,
        description: "Table of output column to expression, for example { acres = \"area * 0.000247\" }",
    }],
    unavailable: None,
    build: || Box::new(ExpressionTransform),
};

const FILTER: OperationSpec = OperationSpec {
    name: "filter",
    description: "Keep only features whose property equals a value",
    parameters: &[
        ParamSpec {
            name: "field",
            param_type: "string",
            required: false,
            default: Some("\"\""),
            description: "Property to test, empty keeps every feature",
        },
        ParamSpec {
            name: "equals",
            param_type: "any",
            required: false,
            default: None,
            description: "Value the property must equal, and it must be the same TOML type",
        },
    ],
    unavailable: None,
    build: || Box::new(FilterTransform),
};

#[cfg(feature = "reproject")]
const REPROJECT: OperationSpec = OperationSpec {
    name: "reproject",
    description: "Transform coordinates between coordinate reference systems",
    parameters: &[
        ParamSpec {
            name: "from_crs",
            param_type: "string",
            required: false,
            default: Some("\"EPSG:4326\""),
            description: "Source CRS, defaults to the input collection's CRS when it has one",
        },
        ParamSpec {
            name: "to_crs",
            param_type: "string",
            required: false,
            default: Some("\"EPSG:3857\""),
            description: "Target CRS, and the CRS the output collection reports",
        },
    ],
    unavailable: None,
    build: || Box::new(ReprojectTransform),
};

const SCHEMA_MAP: OperationSpec = OperationSpec {
    name: "schema_map",
    description: "Rename, drop, and add property columns",
    parameters: &[
        ParamSpec {
            name: "rename",
            param_type: "table",
            required: false,
            default: None,
            description: "Table of old column to new column",
        },
        ParamSpec {
            name: "drop",
            param_type: "array",
            required: false,
            default: None,
            description: "Column names to remove",
        },
        ParamSpec {
            name: "add",
            param_type: "table",
            required: false,
            default: None,
            description: "Table of new column to constant value",
        },
    ],
    unavailable: None,
    build: || Box::new(SchemaMapTransform),
};

const SIMPLIFY: OperationSpec = OperationSpec {
    name: "simplify",
    description: "Reduce vertex count with Douglas-Peucker",
    parameters: &[ParamSpec {
        name: "epsilon",
        param_type: "float",
        required: false,
        default: Some("0.001"),
        description: "Douglas-Peucker tolerance in CRS units, larger removes more vertices",
    }],
    unavailable: None,
    build: || Box::new(SimplifyTransform),
};

const SPATIAL_JOIN: OperationSpec = OperationSpec {
    name: "spatial_join",
    description: "Copy properties from a second dataset onto spatially related features",
    parameters: &[ParamSpec {
        name: "join_type",
        param_type: "string",
        required: false,
        default: Some("\"intersects\""),
        description: "One of intersects, contains, within",
    }],
    // a transform gets one input, so a manifest has no way to name the second
    // dataset. reachable from Rust through SpatialJoinTransform::with_dataset.
    unavailable: Some("a manifest cannot supply the second dataset to join against"),
    build: || Box::new(SpatialJoinTransform::new()),
};

/// Every operation the engine knows, sorted by name.
pub fn operations() -> Vec<OperationSpec> {
    let mut ops = vec![
        BUFFER,
        CENTROID,
        CLIP,
        DISSOLVE,
        EXPRESSION,
        FILTER,
        SCHEMA_MAP,
        SIMPLIFY,
        SPATIAL_JOIN,
    ];
    #[cfg(feature = "reproject")]
    ops.push(REPROJECT);
    ops.sort_by_key(|op| op.name);
    ops
}

/// Look up one operation by the name a manifest would use.
pub fn operation(name: &str) -> Option<OperationSpec> {
    operations().into_iter().find(|op| op.name == name)
}

/// Build the default transform registry with all built-in operations.
pub fn default_registry() -> HashMap<String, Box<dyn TransformOp>> {
    operations()
        .into_iter()
        .map(|op| (op.name.to_string(), (op.build)()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_registry() {
        let reg = default_registry();
        for name in [
            "buffer",
            "centroid",
            "clip",
            "dissolve",
            "expression",
            "filter",
            "schema_map",
            "simplify",
            "spatial_join",
        ] {
            assert!(reg.contains_key(name), "'{name}' is not registered");
        }
        assert_eq!(reg.len(), operations().len());
    }

    /// reproject needs proj, so it is only registered with the feature on.
    #[test]
    fn test_reproject_tracks_its_feature() {
        let registered = default_registry().contains_key("reproject");
        assert_eq!(registered, cfg!(feature = "reproject"));
    }

    #[test]
    fn test_registry_is_built_from_the_operations_table() {
        let registry = default_registry();
        let ops = operations();
        assert_eq!(registry.len(), ops.len());
        for op in &ops {
            assert!(
                registry.contains_key(op.name),
                "'{}' is in the table but not the registry",
                op.name
            );
        }
    }

    #[test]
    fn test_operations_are_sorted_and_unique() {
        let names: Vec<&str> = operations().iter().map(|op| op.name).collect();
        let mut expected = names.clone();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(names, expected);
    }

    #[test]
    fn test_spatial_join_is_the_only_unavailable_operation() {
        let unavailable: Vec<&str> = operations()
            .iter()
            .filter(|op| !op.is_available())
            .map(|op| op.name)
            .collect();
        assert_eq!(unavailable, vec!["spatial_join"]);
    }

    #[test]
    fn test_lookup_by_name() {
        assert_eq!(operation("simplify").unwrap().parameters[0].name, "epsilon");
        assert!(operation("nope").is_none());
    }
}
