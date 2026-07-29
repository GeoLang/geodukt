//! Reading the parameters an operation cannot run without.
//!
//! The registry table marks these `required` and gives them no default, and
//! manifest validation rejects a transform that leaves one out. These readers
//! are the same rule at the point of use, so a caller that builds parameters in
//! Rust fails loudly instead of getting a value it never asked for.

use std::collections::HashMap;

use geodukt_core::pipeline::PipelineError;

use crate::registry::{describe_parameter, operation};

type Params = HashMap<String, toml::Value>;

/// A required number. A manifest may write `500` or `500.0` and mean the same
/// distance, so both are accepted.
pub fn float(params: &Params, op: &str, name: &str) -> Result<f64, PipelineError> {
    let value = require(params, op, name)?;
    value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))
        .ok_or_else(|| wrong_type(op, name, "a number", value))
}

/// A required string.
pub fn string<'a>(params: &'a Params, op: &str, name: &str) -> Result<&'a str, PipelineError> {
    let value = require(params, op, name)?;
    value
        .as_str()
        .ok_or_else(|| wrong_type(op, name, "a string", value))
}

/// A required table, such as a column mapping.
pub fn table<'a>(
    params: &'a Params,
    op: &str,
    name: &str,
) -> Result<&'a toml::value::Table, PipelineError> {
    let value = require(params, op, name)?;
    value
        .as_table()
        .ok_or_else(|| wrong_type(op, name, "a table", value))
}

/// A required parameter of any type.
pub fn require<'a>(
    params: &'a Params,
    op: &str,
    name: &str,
) -> Result<&'a toml::Value, PipelineError> {
    params.get(name).ok_or_else(|| missing(op, name))
}

/// Check the group an operation needs at least one member of, for the operations
/// that have one.
pub fn require_any(params: &Params, op: &str) -> Result<(), PipelineError> {
    let Some(group) = operation(op).and_then(|spec| spec.requires_any) else {
        return Ok(());
    };
    if group
        .parameters
        .iter()
        .any(|name| params.contains_key(*name))
    {
        return Ok(());
    }
    let names: Vec<String> = group.parameters.iter().map(|n| format!("'{n}'")).collect();
    Err(error(
        op,
        format!("needs at least one of {}", names.join(", ")),
    ))
}

fn missing(op: &str, name: &str) -> PipelineError {
    error(
        op,
        format!(
            "missing required parameter {}",
            describe_parameter(op, name)
        ),
    )
}

fn wrong_type(op: &str, name: &str, expected: &str, got: &toml::Value) -> PipelineError {
    error(op, format!("'{name}' must be {expected}, got {got}"))
}

fn error(op: &str, message: String) -> PipelineError {
    PipelineError::Transform {
        name: op.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_float_accepts_an_integer_literal() {
        let params = Params::from([("distance".into(), toml::Value::Integer(500))]);
        assert_eq!(float(&params, "buffer", "distance").unwrap(), 500.0);
    }

    #[test]
    fn test_missing_names_the_parameter_and_its_purpose() {
        let err = float(&Params::new(), "buffer", "distance").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("'distance'"), "{message}");
        assert!(message.contains("in meters"), "{message}");
    }

    #[test]
    fn test_wrong_type_is_an_error_not_a_fallback() {
        let params = Params::from([("distance".into(), toml::Value::String("wide".into()))]);
        let err = float(&params, "buffer", "distance").unwrap_err();
        assert!(err.to_string().contains("must be a number"), "{err}");
    }

    #[test]
    fn test_require_any_is_satisfied_by_one_member() {
        let params = Params::from([("drop".into(), toml::Value::Array(vec![]))]);
        assert!(require_any(&params, "schema_map").is_ok());
        let err = require_any(&Params::new(), "schema_map").unwrap_err();
        assert!(err.to_string().contains("at least one of"), "{err}");
    }

    /// An operation with no such group is fine with anything.
    #[test]
    fn test_require_any_passes_when_there_is_no_group() {
        assert!(require_any(&Params::new(), "centroid").is_ok());
    }
}
