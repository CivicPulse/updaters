use std::{collections::HashSet, io::Write};

use icu_collator::{Collator, CollatorBorrowed, options::CollatorOptions};
use serde_json::Value;

use crate::{Error, Result};

pub(crate) fn collator() -> Result<CollatorBorrowed<'static>> {
    // Explicit English locale matches the pinned reference's ICU localeCompare.
    Collator::try_new(icu_locale::locale!("en").into(), CollatorOptions::default())
        .map_err(|_| Error::Invalid("collation data unavailable"))
}

/// Serialize canonical JSON matching the pinned JavaScript reference.
///
/// Object keys use UTF-16 order; known identity arrays use default ICU collation.
/// Numbers use ECMAScript binary64 serialization, including negative zero.
///
/// # Errors
/// Returns an error for an invalid number, excessive nesting, or I/O failure.
pub fn canonical(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_canonical(value, "", &mut bytes, &collator()?, 0)?;
    Ok(bytes)
}

pub(crate) fn write_canonical(
    value: &Value,
    field: &str,
    out: &mut impl Write,
    collator: &CollatorBorrowed<'_>,
    depth: usize,
) -> Result<()> {
    if depth > 40 {
        return Err(Error::Invalid("source nesting exceeds limit"));
    }
    match value {
        Value::Object(map) => {
            out.write_all(b"{")?;
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    out.write_all(b",")?;
                }
                serde_json::to_writer(&mut *out, key)?;
                out.write_all(b":")?;
                write_canonical(&map[key], key, out, collator, depth + 1)?;
            }
            out.write_all(b"}")?;
        }
        Value::Array(array) => {
            let identity = match field {
                "ballotItems" | "ballotOptions" | "precinctResults" => Some("id"),
                "groupResults" => Some("groupName"),
                _ => None,
            };
            let mut values: Vec<_> = array.iter().collect();
            if let Some(identity) = identity {
                let mut ids = HashSet::new();
                if array.iter().all(|item| {
                    item.get(identity)
                        .and_then(Value::as_str)
                        .is_some_and(|id| ids.insert(id))
                }) {
                    values.sort_by(|a, b| {
                        collator.compare(
                            a[identity].as_str().unwrap_or_default(),
                            b[identity].as_str().unwrap_or_default(),
                        )
                    });
                }
            }
            out.write_all(b"[")?;
            for (index, child) in values.into_iter().enumerate() {
                if index != 0 {
                    out.write_all(b",")?;
                }
                write_canonical(child, "", out, collator, depth + 1)?;
            }
            out.write_all(b"]")?;
        }
        Value::Number(number) => {
            let number = number
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or(Error::Invalid("invalid source number"))?;
            out.write_all(ryu_js::Buffer::new().format_finite(number).as_bytes())?;
        }
        _ => serde_json::to_writer(out, value)?,
    }
    Ok(())
}

/// Serialize JSON using JSON.stringify-compatible numbers and property ordering.
///
/// Non-index object keys keep insertion order; array-index keys enumerate first.
///
/// # Errors
/// Returns errors on excessive nesting or serialization failure.
pub fn stringify(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    write_json(value, &mut bytes, 0)?;
    Ok(bytes)
}

pub(crate) fn write_json(value: &Value, out: &mut impl Write, depth: usize) -> Result<()> {
    if depth > 40 {
        return Err(Error::Invalid("source nesting exceeds limit"));
    }
    match value {
        Value::Object(map) => {
            let index_key = |key: &str| {
                key.parse::<u32>()
                    .ok()
                    .filter(|number| *number != u32::MAX && number.to_string() == key)
            };
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_by_key(|key| index_key(key).map_or((1, 0), |index| (0, index)));
            out.write_all(b"{")?;
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.write_all(b",")?;
                }
                serde_json::to_writer(&mut *out, key)?;
                out.write_all(b":")?;
                write_json(&map[key], out, depth + 1)?;
            }
            out.write_all(b"}")?;
        }
        Value::Array(values) => {
            out.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.write_all(b",")?;
                }
                write_json(value, out, depth + 1)?;
            }
            out.write_all(b"]")?;
        }
        Value::Number(number) => {
            let number = number
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or(Error::Invalid("invalid source number"))?;
            out.write_all(ryu_js::Buffer::new().format_finite(number).as_bytes())?;
        }
        _ => serde_json::to_writer(out, value)?,
    }
    Ok(())
}
