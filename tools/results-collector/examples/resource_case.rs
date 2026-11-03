//! Synthetic near-cap case, streamed one scope at a time; never production results.
use civpulse_results_source::{Expectations, Limits, prepare, stringify};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

struct Scope {
    bytes: usize,
    items: usize,
    capacity: usize,
}
fn padded(path: &Path, extra: usize) -> Result<Value, Box<dyn std::error::Error>> {
    let mut value: Value = serde_json::from_reader(File::open(path)?)?;
    let items = value["ballotItems"].as_array_mut().ok_or("missing items")?;
    let count = items.len();
    if count == 0 && extra != 0 {
        return Err("cannot pad empty scope".into());
    }
    for (index, item) in items.iter_mut().enumerate() {
        let size = extra / count + usize::from(index < extra % count);
        item["resourceTestPadding"] = Value::String("x".repeat(size));
        if stringify(item)?.len() > 2 * 1024 * 1024 - 4096 {
            return Err("synthetic item exceeds safe RPC batch bound".into());
        }
    }
    Ok(value)
}
fn hash(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut chunk = vec![0; 65536];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        digest.update(&chunk[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        return Err("Usage: resource_case INPUT OUTPUT RAW_TARGET_BYTES SCOPE_TARGET_BYTES (create-new only)".into());
    }
    let raw = args[2].parse::<usize>()?;
    let scope_target = args[3].parse::<usize>()?;
    if raw > 512 * 1024 * 1024 || scope_target > 64 * 1024 * 1024 || scope_target > raw {
        return Err("target exceeds contract".into());
    }
    let scratch = tempfile::tempdir()?;
    let expectations = Expectations {
        election_date: "2026-11-03".into(),
        expected_jurisdictions: Some(160),
        expected_items: Some(4178),
    };
    let original = prepare(
        Path::new(&args[0]),
        &scratch.path().join("source"),
        &expectations,
        &Limits::default(),
        &AtomicBool::new(false),
    )?;
    let mut descriptors = Vec::new();
    for scope in &original.scopes {
        let value = padded(&scope.path, 0)?;
        let items = value["ballotItems"].as_array().ok_or("missing items")?;
        let safe_item: usize = 2 * 1024 * 1024 - 4096;
        let item_capacity = items
            .iter()
            .map(|item| stringify(item).map(|b| safe_item.saturating_sub(b.len())))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .min()
            .unwrap_or(0)
            * items.len();
        let bytes = stringify(&value)?.len();
        descriptors.push(Scope {
            bytes,
            items: items.len(),
            capacity: scope_target.saturating_sub(bytes).min(item_capacity),
        });
    }
    let concentrated = descriptors
        .iter()
        .enumerate()
        .skip(1)
        .max_by_key(|(_, s)| s.items)
        .map(|(i, _)| i)
        .ok_or("no county scopes")?;
    let header = stringify(&Value::Object(original.header.clone()))?;
    let baseline = header.len() - 1
        + b",\"results\":".len()
        + b",\"localResults\":[".len()
        + descriptors.len()
        - 2
        + 2
        + descriptors.iter().map(|s| s.bytes).sum::<usize>();
    let mut extra = raw
        .checked_sub(baseline)
        .ok_or("raw target smaller than input")?;
    let mut padding = vec![0; descriptors.len()];
    let concentrated_extra = scope_target
        .checked_sub(descriptors[concentrated].bytes)
        .ok_or("scope target smaller than county")?;
    if concentrated_extra > descriptors[concentrated].capacity || concentrated_extra > extra {
        return Err("concentrated scope cannot fit target".into());
    }
    padding[concentrated] = concentrated_extra;
    extra -= concentrated_extra;
    distribute(&descriptors, &mut padding, concentrated, extra)?;
    let output = Path::new(&args[1]);
    let mut file = File::options().write(true).create_new(true).open(output)?;
    file.write_all(&header[..header.len() - 1])?;
    for (index, scope) in original.scopes.iter().enumerate() {
        file.write_all(if index == 0 {
            b",\"results\":"
        } else if index == 1 {
            b",\"localResults\":["
        } else {
            b","
        })?;
        file.write_all(&stringify(&padded(&scope.path, padding[index])?)?)?;
    }
    file.write_all(b"]}")?;
    file.flush()?;
    if file.metadata()?.len() != raw as u64 {
        return Err("fixture target byte mismatch".into());
    }
    let verified = prepare(
        output,
        &scratch.path().join("verified"),
        &expectations,
        &Limits::default(),
        &AtomicBool::new(false),
    )?;
    println!(
        "{}",
        serde_json::json!({"synthetic_resource_case":true,"production_acceptance":false,"raw_bytes":raw,"raw_sha256":hash(output)?,"largest_scope_bytes":scope_target,"concentrated_scope_index":concentrated,"concentrated_items":descriptors[concentrated].items,"jurisdictions":verified.scopes.len(),"items":verified.item_count})
    );
    Ok(())
}
fn distribute(
    scopes: &[Scope],
    padding: &mut [usize],
    excluded: usize,
    mut remaining: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    while remaining > 0 {
        let available: Vec<_> = scopes
            .iter()
            .enumerate()
            .filter(|(i, s)| *i != excluded && padding[*i] < s.capacity)
            .map(|(i, _)| i)
            .collect();
        if available.is_empty() {
            return Err("raw target exceeds safe scope/item capacities".into());
        }
        let share = remaining.div_ceil(available.len());
        for index in available {
            let add = share
                .min(scopes[index].capacity - padding[index])
                .min(remaining);
            padding[index] += add;
            remaining -= add;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distribution_respects_scope_capacity_and_preserves_concentrated_scope() {
        let scopes = vec![
            Scope {
                bytes: 10,
                items: 1,
                capacity: 3,
            },
            Scope {
                bytes: 10,
                items: 1,
                capacity: 9,
            },
            Scope {
                bytes: 10,
                items: 1,
                capacity: 5,
            },
        ];
        let mut padding = vec![0, 9, 0];
        distribute(&scopes, &mut padding, 1, 8).unwrap();
        assert_eq!(padding, vec![3, 9, 5]);
        assert!(distribute(&scopes, &mut padding, 1, 1).is_err());
    }
}
