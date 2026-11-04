//! Local validation/resource measurement: no credentials or external writes.
use civpulse_results_source::{Expectations, Limits, prepare};
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::args()
        .nth(1)
        .ok_or("usage: validate <source.json>")?;
    let accepted_topologies = std::env::args()
        .nth(2)
        .map(|raw| serde_json::from_str::<Vec<String>>(&raw))
        .transpose()?
        .unwrap_or_default();
    let temp = tempfile::tempdir()?;
    let start = Instant::now();
    let prepared = prepare(
        Path::new(&input),
        &temp.path().join("scopes"),
        &Expectations {
            election_date: "2026-11-03".to_owned(),
            expected_jurisdictions: Some(160),
            expected_items: Some(4_178),
            accepted_topologies,
        },
        &Limits {
            max_download: 128 * 1024 * 1024,
            max_scope: 16 * 1024 * 1024,
            ..Limits::default()
        },
        &AtomicBool::new(false),
    )?;
    println!(
        "jurisdictions={} items={} fingerprint={} topology_sha256={} duration_ms={}",
        prepared.scopes.len(),
        prepared.item_count,
        prepared.fingerprint,
        prepared.topology_sha256,
        start.elapsed().as_millis()
    );
    Ok(())
}
