use anyhow::{Result, bail};
use serde_json::Value;
use std::sync::OnceLock;

/// The frozen V2 tool catalog.
///
/// # Panics
/// Panics only if the bundled, checked catalog is malformed.
pub fn tools() -> &'static Vec<Value> {
    static TOOLS: OnceLock<Vec<Value>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        serde_json::from_str(include_str!("../reference/tools.json")).expect("embedded catalog")
    })
}
pub(crate) fn legislation() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../reference/legislation.json"))
            .expect("embedded legislation")
    })
}
/// Apply the input defaults exported from Zod before JSON Schema validation.
pub(crate) fn defaults(schema: &Value, value: &mut Value) {
    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        for variant in variants {
            let mut candidate = value.clone();
            defaults(variant, &mut candidate);
            if validator(variant).is_ok_and(|v| v.is_valid(&candidate)) {
                *value = candidate;
                break;
            }
        }
    }
    if let (Some(props), Some(object)) = (schema["properties"].as_object(), value.as_object_mut()) {
        for (key, property) in props {
            if !object.contains_key(key) {
                if let Some(default) = property.get("default") {
                    object.insert(key.clone(), default.clone());
                }
            }
            if let Some(field) = object.get_mut(key) {
                defaults(property, field);
            }
        }
        // Zod's ordinary object schemas strip unknown fields. Strict schemas reject them.
        if schema.get("additionalProperties").is_none() {
            object.retain(|key, _| props.contains_key(key));
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array_mut()) {
        for item in array {
            defaults(items, item);
        }
    }
}
fn validator(schema: &Value) -> Result<jsonschema::Validator> {
    Ok(jsonschema::options()
        .should_validate_formats(true)
        .build(schema)?)
}
pub(crate) fn validate(schema: &Value, value: &mut Value) -> Result<()> {
    defaults(schema, value);
    let validator = validator(schema)?;
    if let Err(error) = validator.validate(value) {
        bail!("Invalid arguments: {error}");
    }
    Ok(())
}
fn trim_path(value: &mut Value, path: &[Value]) {
    let Some((key, rest)) = path.split_first() else {
        if let Some(s) = value.as_str() {
            *value = Value::String(s.trim().to_owned());
        }
        return;
    };
    if key == "*" {
        if let Some(rows) = value.as_array_mut() {
            for row in rows {
                trim_path(row, rest);
            }
        }
    } else if let Some(v) = key.as_str().and_then(|k| value.get_mut(k)) {
        trim_path(v, rest);
    }
}
pub(crate) fn arguments(name: &str, mut args: Value) -> Result<Value> {
    let normalization: Value =
        serde_json::from_str(include_str!("../reference/normalization.json"))?;
    if let Some(paths) = normalization[name].as_array() {
        for path in paths {
            trim_path(&mut args, path.as_array().expect("trim path"));
        }
    }
    let tool = tools()
        .iter()
        .find(|t| t["name"] == name)
        .ok_or_else(|| anyhow::anyhow!("Unknown tool: {name}"))?;
    validate(&tool["inputSchema"], &mut args)?;
    Ok(args)
}
