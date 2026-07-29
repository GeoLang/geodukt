//! Transform registry — the one table of operations the pipeline engine can run.
//!
//! [`default_registry`] builds the executor's dispatch map from [`operations`],
//! and the server's catalog endpoint reads the same table, so what the API
//! advertises and what the engine runs cannot drift apart. Adding an operation
//! means adding one row here.

use std::collections::HashMap;

use geodukt_core::manifest::Manifest;
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
    /// True when the manifest has to supply it, because the operation has no
    /// value that could stand in for what the caller meant. Such a parameter
    /// carries no default, and leaving it out is rejected before the run.
    pub required: bool,
    /// The literal TOML value used when the parameter is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<&'static str>,
    pub description: &'static str,
}

impl ParamSpec {
    /// How a rejection names this parameter: what to add, and what it is for.
    fn describe(&self) -> String {
        format!("'{}' ({})", self.name, self.description)
    }
}

/// Parameters an operation needs at least one of, where no single one of them is
/// required on its own.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RequiresAny {
    pub parameters: &'static [&'static str],
    /// Why one of them has to be there, for the rejection message.
    pub purpose: &'static str,
}

/// One operation, its parameters, and how to construct it.
#[derive(Clone, Copy, Serialize)]
pub struct OperationSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: &'static [ParamSpec],
    /// A group the manifest has to supply at least one member of, when no single
    /// parameter is required by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_any: Option<RequiresAny>,
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
            .field("requires_any", &self.requires_any)
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

impl OperationSpec {
    /// True when a manifest may use this operation.
    pub fn is_available(&self) -> bool {
        self.unavailable.is_none()
    }

    /// Why these parameters do not let the operation run, when they do not.
    /// Presence only: a required parameter has no default to fall back on, so
    /// leaving it out cannot be absorbed silently.
    pub fn unmet_requirement(&self, params: &HashMap<String, toml::Value>) -> Option<String> {
        let missing: Vec<String> = self
            .parameters
            .iter()
            .filter(|p| p.required && !params.contains_key(p.name))
            .map(|p| p.describe())
            .collect();
        if !missing.is_empty() {
            let noun = if missing.len() == 1 {
                "parameter"
            } else {
                "parameters"
            };
            return Some(format!("missing required {noun} {}", missing.join(", ")));
        }

        let group = self.requires_any?;
        if group
            .parameters
            .iter()
            .any(|name| params.contains_key(*name))
        {
            return None;
        }
        let names: Vec<String> = group.parameters.iter().map(|n| format!("'{n}'")).collect();
        Some(format!(
            "needs at least one of {} ({})",
            names.join(", "),
            group.purpose
        ))
    }
}

const BUFFER: OperationSpec = OperationSpec {
    name: "buffer",
    description: "Expand or shrink geometries by a distance in meters, with round joins",
    parameters: &[
        ParamSpec {
            name: "distance",
            param_type: "float",
            required: true,
            default: None,
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
    requires_any: None,
    unavailable: None,
    build: || Box::new(BufferTransform),
};

const CENTROID: OperationSpec = OperationSpec {
    name: "centroid",
    description: "Replace each geometry with its centroid point",
    parameters: &[],
    requires_any: None,
    unavailable: None,
    build: || Box::new(CentroidTransform),
};

// the bounding box is the only clip boundary a manifest can express, so all four
// edges are required. ClipTransform::with_boundary takes a polygon instead, and
// that is reachable from Rust only.
const CLIP: OperationSpec = OperationSpec {
    name: "clip",
    description: "Intersect geometries with a bounding box, dropping what falls outside",
    parameters: &[
        ParamSpec {
            name: "min_x",
            param_type: "float",
            required: true,
            default: None,
            description: "West edge of the clip box in CRS units",
        },
        ParamSpec {
            name: "min_y",
            param_type: "float",
            required: true,
            default: None,
            description: "South edge of the clip box in CRS units",
        },
        ParamSpec {
            name: "max_x",
            param_type: "float",
            required: true,
            default: None,
            description: "East edge of the clip box in CRS units",
        },
        ParamSpec {
            name: "max_y",
            param_type: "float",
            required: true,
            default: None,
            description: "North edge of the clip box in CRS units",
        },
    ],
    requires_any: None,
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
    requires_any: None,
    unavailable: None,
    build: || Box::new(DissolveTransform),
};

const EXPRESSION: OperationSpec = OperationSpec {
    name: "expression",
    description: "Add computed property columns from geometry measures and arithmetic",
    parameters: &[ParamSpec {
        name: "expressions",
        param_type: "table",
        required: true,
        default: None,
        description: "Table of new column to expression, for example { acres = \"$area / 4047\" }, where an expression is one of $area, $length, $num_vertices, $geom_type, $prop.<column>, or <column> with * / + - and a number",
    }],
    requires_any: None,
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
            required: true,
            default: None,
            description: "Property to test",
        },
        ParamSpec {
            name: "equals",
            param_type: "any",
            required: true,
            default: None,
            description: "Value the property must equal, and it must be the same TOML type",
        },
    ],
    requires_any: None,
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
            required: true,
            default: None,
            description: "Target CRS, and the CRS the output collection reports",
        },
    ],
    requires_any: None,
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
            description: "Table keyed by the column the input already has, valued with the name it gets, for example { pop_2020 = \"population\" } renames pop_2020 to population",
        },
        ParamSpec {
            name: "drop",
            param_type: "array",
            required: false,
            default: None,
            description: "Column names to remove, for example [\"shape_area\"]",
        },
        ParamSpec {
            name: "add",
            param_type: "table",
            required: false,
            default: None,
            description: "Table of new column to the constant value every feature gets, for example { source = \"census\" }",
        },
    ],
    requires_any: Some(RequiresAny {
        parameters: &["rename", "drop", "add"],
        purpose: "a schema_map that renames, drops, and adds nothing leaves the columns as they are",
    }),
    unavailable: None,
    build: || Box::new(SchemaMapTransform),
};

const SIMPLIFY: OperationSpec = OperationSpec {
    name: "simplify",
    description: "Reduce vertex count with Douglas-Peucker",
    parameters: &[ParamSpec {
        name: "epsilon",
        param_type: "float",
        required: true,
        default: None,
        description: "Douglas-Peucker tolerance in CRS units, larger removes more vertices",
    }],
    requires_any: None,
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
    requires_any: None,
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

/// How a rejection names one parameter of an operation: what to add, and what it
/// is for. Falls back to the bare name for a parameter the table does not list.
pub fn describe_parameter(op: &str, name: &str) -> String {
    operation(op)
        .and_then(|spec| spec.parameters.iter().find(|p| p.name == name).copied())
        .map(|p| p.describe())
        .unwrap_or_else(|| format!("'{name}'"))
}

/// Reject a transform that leaves out a parameter its operation cannot run
/// without, giving back the message a caller reports. An operation the table
/// does not know is somebody else's rejection.
pub fn check_parameters(manifest: &Manifest) -> Result<(), String> {
    for transform in &manifest.transform {
        let Some(spec) = operation(&transform.operation) else {
            continue;
        };
        if let Some(reason) = spec.unmet_requirement(&transform.params) {
            return Err(format!(
                "transform '{}' uses operation '{}' which cannot run: {reason}",
                transform.name, transform.operation
            ));
        }
    }
    Ok(())
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

    /// A parameter the manifest has to supply cannot also have a value that
    /// stands in for it, or the requirement is a lie.
    #[test]
    fn test_no_required_parameter_carries_a_default() {
        for op in operations() {
            for param in op.parameters {
                assert!(
                    !(param.required && param.default.is_some()),
                    "{}.{} is required and defaults to {:?}",
                    op.name,
                    param.name,
                    param.default
                );
            }
        }
    }

    #[test]
    fn test_missing_required_parameter_names_it_and_its_purpose() {
        let buffer = operation("buffer").unwrap();
        let reason = buffer.unmet_requirement(&HashMap::new()).unwrap();
        assert!(reason.contains("'distance'"), "{reason}");
        assert!(reason.contains("in meters"), "{reason}");

        let params = HashMap::from([("distance".into(), toml::Value::Float(500.0))]);
        assert!(buffer.unmet_requirement(&params).is_none());
    }

    /// All four edges or none: three of them describe no box.
    #[test]
    fn test_clip_reports_every_edge_it_is_missing() {
        let clip = operation("clip").unwrap();
        let params = HashMap::from([
            ("min_x".to_string(), toml::Value::Float(0.0)),
            ("min_y".to_string(), toml::Value::Float(0.0)),
        ]);
        let reason = clip.unmet_requirement(&params).unwrap();
        assert!(reason.contains("'max_x'"), "{reason}");
        assert!(reason.contains("'max_y'"), "{reason}");
        assert!(!reason.contains("'min_x'"), "{reason}");
    }

    #[test]
    fn test_schema_map_needs_one_of_its_three() {
        let schema_map = operation("schema_map").unwrap();
        let reason = schema_map.unmet_requirement(&HashMap::new()).unwrap();
        assert!(reason.contains("at least one of"), "{reason}");

        for name in ["rename", "drop", "add"] {
            let params = HashMap::from([(name.to_string(), toml::Value::Array(vec![]))]);
            assert!(schema_map.unmet_requirement(&params).is_none(), "{name}");
        }
    }

    #[test]
    fn test_check_parameters_names_the_transform_and_the_operation() {
        let manifest = Manifest::from_toml(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "wide"
input = "src"
operation = "buffer"
"#,
        )
        .unwrap();
        let message = check_parameters(&manifest).unwrap_err();
        assert!(message.contains("transform 'wide'"), "{message}");
        assert!(message.contains("operation 'buffer'"), "{message}");
        assert!(message.contains("'distance'"), "{message}");
    }

    /// An operation the table does not know is rejected elsewhere, by name.
    #[test]
    fn test_check_parameters_ignores_an_unknown_operation() {
        let manifest = Manifest::from_toml(
            r#"
[project]
name = "p"

[[source]]
name = "src"
format = "geojson"
path = "a.geojson"

[[transform]]
name = "oops"
input = "src"
operation = "buffr"
"#,
        )
        .unwrap();
        assert!(check_parameters(&manifest).is_ok());
    }

    #[test]
    fn test_lookup_by_name() {
        assert_eq!(operation("simplify").unwrap().parameters[0].name, "epsilon");
        assert!(operation("nope").is_none());
    }
}
